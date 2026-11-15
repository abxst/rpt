//! Strict, read-only subset of APT's apt.conf grammar. This supports
//! nested scopes and multiple statements per line without silently ignoring
//! settings that could change candidate policy, source roots or dpkg behavior.
//! It is NOT general APT configuration compatibility.
use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Quoted(String),
    Open,
    Close,
    Semi,
}

fn tokenize(input: &str) -> Result<Vec<Token>> {
    if input.len() > 1024 * 1024 {
        bail!("APT config file exceeds 1 MiB");
    }
    let raw = input.as_bytes();
    let mut index = 0;
    let mut out = Vec::new();
    while index < raw.len() {
        if raw[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if raw[index] == b'#' {
            let end = (index..raw.len()).find(|&i| raw[i] == b'\n').unwrap_or(raw.len());
            let directive = &input[index..end].trim().to_ascii_lowercase();
            if directive.starts_with("#include") || directive.starts_with("#includedir") {
                bail!("APT configuration #include/#includedir is unsupported");
            }
            index = end;
            continue;
        }
        if raw[index..].starts_with(b"//") {
            index = (index..raw.len()).find(|&i| raw[i] == b'\n').unwrap_or(raw.len());
            continue;
        }
        if raw[index..].starts_with(b"/*") {
            let Some(end) = (index + 2..raw.len().saturating_sub(1))
                .find(|&i| raw[i..].starts_with(b"*/")) else {
                bail!("unterminated APT config block comment");
            };
            index = end + 2;
            continue;
        }
        let token = match raw[index] {
            b'{' => { index += 1; Token::Open }
            b'}' => { index += 1; Token::Close }
            b';' => { index += 1; Token::Semi }
            b'"' => {
                index += 1;
                let mut value = Vec::new();
                let mut closed = false;
                while index < raw.len() {
                    match raw[index] {
                        b'"' => { index += 1; closed = true; break; }
                        b'\\' => {
                            index += 1;
                            if index >= raw.len() || !matches!(raw[index], b'"' | b'\\') {
                                bail!("unsupported APT configuration quote escape");
                            }
                            value.push(raw[index]);
                            index += 1;
                        }
                        b'\n' | b'\r' | 0 => bail!("invalid newline or NUL inside APT config quoted value"),
                        other => { value.push(other); index += 1; }
                    }
                    if value.len() > 8192 { bail!("APT config string exceeds 8 KiB"); }
                }
                if !closed { bail!("unterminated APT config string"); }
                Token::Quoted(String::from_utf8(value)?)
            }
            _ => {
                let start = index;
                while index < raw.len() && (raw[index].is_ascii_alphanumeric()
                    || matches!(raw[index], b':' | b'.' | b'-' | b'_')) {
                    index += 1;
                }
                if start == index || index - start > 128 {
                    bail!("unsupported APT config token at byte offset {start}");
                }
                Token::Word(input[start..index].to_owned())
            }
        };
        out.push(token);
        if out.len() > 4096 { bail!("APT config token limit exceeded"); }
    }
    Ok(out)
}

#[derive(Default)]
struct State {
    target_release: Option<String>,
}

fn add_leaf(state: &mut State, scope: &[String], key: &str, value: &str) -> Result<()> {
    let mut name = scope.iter().map(|part| part.to_ascii_lowercase()).collect::<Vec<_>>();
    let pieces = key.split("::").collect::<Vec<_>>();
    if pieces.iter().any(|part| part.is_empty()) {
        bail!("empty APT configuration key segment");
    }
    name.extend(pieces.into_iter().map(str::to_ascii_lowercase));
    match name.join("::").as_str() {
        "apt::default-release" => {
            if value.is_empty() || value.len() > 128 || !value.bytes().all(|b|
                b.is_ascii_alphanumeric() || b".+_-".contains(&b)) {
                bail!("unsupported APT::Default-Release value");
            }
            if state.target_release.is_some() {
                bail!("duplicate APT::Default-Release declaration");
            }
            state.target_release = Some(value.to_owned());
        }
        // Output-only settings accepted as harmless for READ-ONLY candidate
        // selection, not modeled for CLI or dpkg execution behavior.
        "dpkg::progress-fancy" | "apt::color" => {
            if !matches!(value.to_ascii_lowercase().as_str(),
                "0" | "1" | "yes" | "no" | "true" | "false") {
                bail!("unsupported APT cosmetic boolean: {value}");
            }
        }
        _ => bail!("unsupported APT config setting: {}", name.join("::")),
    }
    Ok(())
}

fn consume_scope(tokens: &[Token], position: &mut usize, prefix: &mut Vec<String>,
    state: &mut State, depth: usize, top: bool) -> Result<()> {
    if depth > 16 { bail!("APT configuration nesting depth exceeded"); }
    loop {
        let Some(next) = tokens.get(*position) else {
            if top { return Ok(()); }
            bail!("unclosed APT configuration scope");
        };
        if *next == Token::Close {
            if top { bail!("unexpected APT configuration close brace"); }
            *position += 1;
            return Ok(());
        }
        let key = match next {
            Token::Word(key) => key.clone(),
            _ => bail!("expected APT config key"),
        };
        *position += 1;
        match tokens.get(*position) {
            Some(Token::Open) => {
                *position += 1;
                let pieces = key.split("::").collect::<Vec<_>>();
                if pieces.iter().any(|part| part.is_empty()) {
                    bail!("invalid APT configuration scope");
                }
                let old_len = prefix.len();
                prefix.extend(pieces.into_iter().map(str::to_owned));
                consume_scope(tokens, position, prefix, state, depth + 1, false)?;
                prefix.truncate(old_len);
                if tokens.get(*position) != Some(&Token::Semi) {
                    bail!("APT configuration scope missing ';'");
                }
                *position += 1;
            }
            Some(Token::Quoted(value)) | Some(Token::Word(value)) => {
                add_leaf(state, prefix, &key, value)?;
                *position += 1;
                if tokens.get(*position) != Some(&Token::Semi) {
                    bail!("APT configuration assignment missing ';'");
                }
                *position += 1;
            }
            _ => bail!("expected APT config assignment or scope"),
        }
    }
}

/// Parse only unambiguous policy-impacting target release and a small set
/// of nonpolicy cosmetic options. Unknown active APT settings fail closed.
pub fn target_release(input: &str) -> Result<Option<String>> {
    let tokens = tokenize(input)?;
    let mut position = 0usize;
    let mut state = State::default();
    consume_scope(&tokens, &mut position, &mut Vec::new(), &mut state, 0, true)?;
    if position != tokens.len() {
        bail!("unconsumed APT configuration tokens");
    }
    Ok(state.target_release)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_nested_and_flat_apts_with_comments() {
        for syntax in [
            "APT::Default-Release \"noble\";",
            "APT { Default-Release \"noble\"; };",
            "/* comment */ APT { // inline\n Default-Release \"noble\"; };",
            "APT {Default-Release \"noble\";}; Dpkg::Progress-Fancy \"true\";",
            "Dpkg { Progress-Fancy \"true\"; }; APT::Default-Release \"noble\";",
            "# comment\nAPT::Default-Release noble;",
        ] {
            assert_eq!(target_release(syntax).unwrap().as_deref(), Some("noble"), "{syntax}");
        }
        assert_eq!(target_release("Dpkg::Progress-Fancy \"false\";").unwrap(), None);
    }

    #[test]
    fn rejects_indirections_and_unknown_policy_keys() {
        for invalid in [
            "#include \"/tmp/custom\";",
            "#includedir \"/tmp/apt.conf.d\";",
            "Dir::Etc::preferences \"other\";",
            "APT::Install-Recommends \"true\";",
            "Acquire::Retries \"4\";",
            "APT::Default-Release \"noble*\";",
            "APT::Default-Release \"noble\"; APT::Default-Release \"noble-updates\";",
            "APT { Default-Release \"noble\"; }; APT::Unknown \"value\";",
            "APT { Default-Release \"noble\"; }",
            "APT { Default-Release \"noble\";",
            "APT::Default-Release \"unterminated;",
            "APT::Default-Release \"noble\"; */",
            "/* unterminated",
            "APT::Default-Release \"noble\"; }",
            "APT::Default-Release \"noble\"; APT::Color \"maybe\";",
            "APT::Default-Release \"noble\"; APT::Color \"1\"; APT::Get::Assume-Yes \"true\";",
        ] {
            assert!(target_release(invalid).is_err(), "must reject {invalid}");
        }
    }

    #[test]
    fn nested_and_flat_paths_cannot_bypass_duplicate_detection() {
        assert!(target_release(
            "APT::Default-Release \"noble\"; APT { Default-Release \"noble-updates\"; };",
        ).is_err());
        assert!(target_release(
            "APT { Default-Release \"noble\"; Dpkg::Progress-Fancy \"true\"; };",
        ).is_err());
    }
}

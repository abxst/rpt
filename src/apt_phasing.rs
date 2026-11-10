//! Exact APT phased-update draw for libstdc++ std::seed_seq,
//! std::minstd_rand and std::uniform_int_distribution(0, 99).
//! Only used with an explicit machine ID. No host or configuration inference.
use anyhow::{bail, Result};

pub fn rollout_draw(source: &str, version: &str, machine_id: &str) -> Result<u8> {
    if source.is_empty() || version.is_empty() || machine_id.is_empty()
        || source.len() > 256 || version.len() > 512 || machine_id.len() > 128
        || !machine_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        bail!("invalid phased update source/version/machine ID");
    }
    let seed = format!("{source}-{version}-{machine_id}");
    let input = seed.as_bytes();
    // minstd_rand draws one uint32 from seed_seq, requesting k+3=4 values.
    let mut state = [0x8b8b8b8bu32; 4];
    let s = input.len();
    let m = (s + 1).max(4);
    const R: u32 = 1371501266;
    state[1] = state[1].wrapping_add(R);
    state[2] = state[2].wrapping_add(R.wrapping_add(s as u32));
    state[0] = R.wrapping_add(s as u32);
    for k in 1..(m + 4) {
        let kn = k % 4;
        let kp = (k + 1) % 4;
        let kq = (k + 2) % 4;
        if k < m {
            let arg = state[kn] ^ state[kp] ^ state[(k - 1) % 4];
            let r1 = 1664525u32.wrapping_mul(arg ^ (arg >> 27));
            let r2 = r1.wrapping_add(kn as u32)
                .wrapping_add(if k <= s { u32::from(input[k - 1]) } else { 0 });
            state[kp] = state[kp].wrapping_add(r1);
            state[kq] = state[kq].wrapping_add(r2);
            state[kn] = r2;
        } else {
            let arg = state[kn].wrapping_add(state[kp])
                .wrapping_add(state[(k - 1) % 4]);
            let r3 = 1566083941u32.wrapping_mul(arg ^ (arg >> 27));
            let r4 = r3.wrapping_sub(kn as u32);
            state[kp] ^= r3;
            state[kq] ^= r4;
            state[kn] = r4;
        }
    }
    const MOD: u64 = 2147483647;
    const MULT: u64 = 48271;
    // libstdc++ minstd_rand::seed(seed_seq) uses result[3] modulo MOD.
    let mut x = u64::from(state[3]) % MOD;
    if x == 0 { x = 1; }
    loop {
        x = x * MULT % MOD;
        // libstdc++ uniform_int_distribution(0,99) uses downscale by
        // floor((minstd_rand.max-min)/100), rejecting the tail.
        let value = x - 1;
        if value < 2147483600 {
            return Ok((value / 21474836) as u8);
        }
    }
}

pub fn eligible(source: &str, version: &str, machine_id: &str, percent: u8) -> Result<bool> {
    if percent > 100 { bail!("invalid phased percentage"); }
    Ok(rollout_draw(source, version, machine_id)? < percent)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matches_libstdcpp_apt_reference_vectors() {
        // Generated using std::seed_seq, std::minstd_rand,
        // std::uniform_int_distribution<unsigned>(0,99) on Linux.
        for (source, version, machine, expected) in [
            ("bash", "5.2.21-2ubuntu4", "8de277067b3544d4b65c267d0edab928", 6),
            ("foo", "1.0", "00000000000000000000000000000000", 52),
            ("foo", "1.0", "11111111111111111111111111111111", 18),
            ("linux", "6.8.0-1", "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", 99),
            ("a", "1", "12345678-1234-1234-1234-123456789abc", 62),
        ] {
            assert_eq!(rollout_draw(source, version, machine).unwrap(), expected);
        }
        assert!(eligible("foo", "1.0", "00000000000000000000000000000000", 53).unwrap());
        assert!(!eligible("foo", "1.0", "00000000000000000000000000000000", 52).unwrap());
        assert!(eligible("foo", "1.0", "00000000000000000000000000000000", 100).unwrap());
        assert!(!eligible("foo", "1.0", "00000000000000000000000000000000", 0).unwrap());
        assert!(eligible("foo", "1.0", "00000000000000000000000000000000", 101).is_err());
    }
}

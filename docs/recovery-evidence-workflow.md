# Recovery evidence workflow v1

This is an **operator evidence capture** workflow, not automatic recovery and not a dpkg transaction executor.

1. Inspect blocked journals: `rpt recovery-inspect --journal-dir ./rpt-journal`.
2. Investigate interrupted processes, mount state, dpkg locks and `dpkg --audit` independently. Capture output in an external incident record.
3. Record the operator assertion: `rpt recovery-attest --journal-dir ./rpt-journal --id tx --operator admin --reason "manual review" --evidence "incident INC-123"`.
4. The command creates `recovery-audit/tx.json` with the journal SHA-256, timestamp and operator statement. It refuses duplicate attestations.
5. **The journal remains blocked**. This command does not clear, rename or repair transaction records. Do not delete blocked journals to bypass the gate.

Limitations: operator identity is self-reported, evidence is a reference/string rather than cryptographic proof, and the audit record is not tamper-evident against a privileged attacker. A future controlled unlock mechanism requires independently verified dpkg/filesystem invariants and a stronger audit model.

# Operator decisions (open as of 2026-11-19)

The issues assume the **recommended** option, marked (R). If the operator picks otherwise, the named issue changes as noted.

| Q | Decision | Options | Affects |
|---|---|---|---|
| Q1 | Engine storage now | **(R)** builds-PVC `/tmp` and `/cache`, `RUSTUP_AUTO_INSTALL=0`, engine ephemeral 16Gi until P1.8 · only raise the limit · pause host workers until phase 1 | P0.1 |
| Q2 | Who may run on the host; dev cap | **(R)** oversight, interactive, shell, agent tasks and the assistant only; workers and reviews refused; cap 4 · the same with cap 8 · allow workers behind the override flag during the transition | P1.8, P1.10 |
| Q3 | Account capacity (only acct2) | **(R)** `ENGINE_AGENT_CLAUDE_ACCOUNT_CAP=12` on dev plus P1.5 · keep 8 · bring a second account forward (WI-3159) | P1.5, P1.10, P1.12 |
| Q4 | Rust worker pod size | **(R)** limits 4 CPU / 8Gi / 30Gi, requests 1 / 2Gi / 10Gi; quota memory 64Gi, ephemeral 240Gi · keep 6Gi/20Gi · decide after the P1.6 measurement | P1.6 |
| Q5 | Caches in phase 1 | **(R)** cold registry per pod plus read-only kache; seed in P2.2 · per-session kache write keys now · registry seed PVC now | P1.6, P2.2 |
| Q6 | Session-pod loss (WI-3370) | **(R)** phase 2 right after P1.12 · pull WI-4267/4268 into phase 1 and add a "delete a session pod" step to P1.12 | P1.12, P2.1 |
| Q7 | Closures | **(R)** as in `closures.md` · only the "now" group | closures |
| Q8 | Graft sources on dev projects | **(R)** every project that lands through graft · vogt only | P1.2 |
| Q9 | Deploy windows | **(R)** deploy as each group is ready; P1.12 in an announced window · one combined deploy | all deploys |

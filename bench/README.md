# API benchmark

`run.sh` starts a throwaway Ring server (its own config directory, database
and port, Docker as its only runtime), load-tests a few routes with
[oha](https://github.com/hatoo/oha) and prints requests/s and p50/p99 for
each. It is for comparing a change against `main` on your machine, not for CI:
shared runners are too noisy for latency numbers.

```sh
cargo install oha

git switch main && OUT=bench/results/before ./bench/run.sh
git switch my-branch && OUT=bench/results/after COMPARE=bench/results/before ./bench/run.sh
```

| Scenario | Request | What it exercises |
| --- | --- | --- |
| `healthz` | `GET /healthz` | the HTTP stack alone: no auth, no database |
| `deployments-session` | `GET /deployments`, login token | auth by session, a list query |
| `deployments-pat` | `GET /deployments`, `ring_pat_` token | auth by API token (hash lookup, scopes) |
| `namespaces-session` | `GET /namespaces`, login token | another list query |
| `login` | `POST /login`, 4 connections | Argon2 password check, slow by design |

The database is empty: the numbers measure the request path, not how lists
scale with their size.

Two runs of the same build differ by a few percent in requests/s and up to
10 to 15% in p99. Run each side two or three times before trusting a smaller
difference, with nothing else loading the CPU.

To see where the time goes in one scenario, profile it with Linux perf and
pima:

```sh
PROFILE=deployments-session ONLY=deployments-session ./bench/run.sh
```

The samples are summarized by `pima perf`, and uploaded when `PIMA_TOKEN` and
`PIMA_PROJECT` are set, where two profiles can be compared. The binary is
built with frame pointers (in `target/bench`) so that perf sees whole stacks.
See `./bench/run.sh` for every setting.

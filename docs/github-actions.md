# GitHub Actions

Workflow: [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)

| Trigger | Jobs |
|---------|------|
| Pull request | `test` — frontend and Python checks, Rust tests, and a production artifact build |
| Push to `main` | `test`, then `deploy` — publish the Rust and frontend artifact produced by the same run |

`deploy.sh` checks both domains and confirms that Flask is inactive and disabled, with no nginx upstream or listener on port 5070, before and after activating the release.

## One-time: deploy secret

Production deploy uses SSH (same as local `./deploy.sh`). Add a **repository secret**:

| Secret | Value |
|--------|--------|
| `DEPLOY_SSH_KEY` | Private key that can `ssh root@164.92.147.23` (ed25519, full PEM including `BEGIN`/`END` lines) |

GitHub → **Settings → Secrets and variables → Actions → New repository secret**.

Optional **variables** (defaults shown in the workflow):

| Variable | Default |
|----------|---------|
| `DEPLOY_HOST` | `164.92.147.23` |
| `DEPLOY_SSH_PORT` | `22` |

The `production` environment on `deploy` allows adding required reviewers later (Settings → Environments).

Manual deploy without CI: run **Actions → CI → Run workflow** on `main`, or locally run `./scripts/test.sh` and then `./deploy.sh`. See the [release procedure](rust-migration.md#публикация-и-откат).

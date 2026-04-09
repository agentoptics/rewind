# Rewind Assert — GitHub Action

Run agent regression tests in CI with [Rewind](https://github.com/agentoptics/rewind). Check recorded sessions against baselines for step-level regressions — step types, models, tool calls, token usage, and error status.

## Quick Start

```yaml
- uses: agentoptics/rewind/action@v1
  with:
    baseline: "booking-happy-path"
```

## Full Example

```yaml
name: Agent Regression Tests
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"

      - name: Install dependencies
        run: pip install rewind-agent openai  # + your agent deps

      - name: Run agent
        env:
          OPENAI_API_KEY: ${{ secrets.OPENAI_API_KEY }}
        run: python my_agent.py

      - name: Check for regressions
        uses: agentoptics/rewind/action@v1
        with:
          baseline: "booking-happy-path"
          token-tolerance: "20"
```

## How It Works

1. Your agent runs in a previous step — `rewind_agent.init()` records all LLM calls to `~/.rewind/`
2. This action runs `rewind assert check` to compare the new session against a baseline
3. If regressions are found (wrong step types, new errors, token spikes), the step fails
4. Results are written to the GitHub Step Summary for easy review

## Creating Baselines

Before using in CI, create a baseline from a known-good session:

```bash
# Run your agent locally
python my_agent.py

# Create baseline from the recording
rewind assert baseline latest --name "booking-happy-path"
```

The baseline is stored in `~/.rewind/rewind.db`. To use in CI, either:
- **Option A:** Commit the baseline database (small, SQLite) and set `rewind-data` to the path
- **Option B:** Run the baseline creation step in CI from a known-good commit

## Inputs

| Input | Required | Default | Description |
|:------|:---------|:--------|:------------|
| `baseline` | Yes | — | Baseline name to check against |
| `session` | No | `latest` | Session to check (ID, prefix, or "latest") |
| `token-tolerance` | No | `20` | Token tolerance percentage |
| `warn-model-change` | No | `false` | Treat model changes as warnings |
| `rewind-data` | No | `~/.rewind` | Path to Rewind data directory |
| `version` | No | `latest` | Rewind CLI version to install |

## Outputs

| Output | Description |
|:-------|:------------|
| `result` | `passed` or `failed` |
| `summary` | Human-readable result summary |

## Multiple Baselines

Test different agent scenarios in the same workflow:

```yaml
- name: Check booking flow
  uses: agentoptics/rewind/action@v1
  with:
    baseline: "booking-happy-path"

- name: Check error handling
  uses: agentoptics/rewind/action@v1
  with:
    baseline: "booking-error-recovery"
```

## Custom Data Directory

If your agent writes to a custom Rewind data directory:

```yaml
- name: Run agent
  env:
    REWIND_DATA: ${{ github.workspace }}/.rewind-ci
  run: python my_agent.py

- uses: agentoptics/rewind/action@v1
  with:
    baseline: "my-baseline"
    rewind-data: ${{ github.workspace }}/.rewind-ci
```

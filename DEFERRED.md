# Deferred

One line each; not tasks (see the work-generation policy).

- HTTP listener: an HTTP/2 client that answers PINGs but never sends a request holds a connection permit indefinitely (hyper 1.8 has no h2 idle-connection timeout; `keep_alive_while_idle` only detects dead peers). Revisit if hyper adds one or an idle-reaper is wanted.
- release-plz `workflow_dispatch` (#283): a manual recovery path for main-branch pushes that started no workflows, because auto-merge merged with `GITHUB_TOKEN` (macp-ci#1). macp-ci v1.1 now merges with the macp-deps-bot token, so the path is redundant. It stays as a manual recovery tool. Revisit removing it once a bot-armed Dependabot merge is seen starting push runs (CI, release-plz) on main.

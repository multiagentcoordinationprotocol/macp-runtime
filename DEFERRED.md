# Deferred

One line each; not tasks (see the work-generation policy).

- HTTP listener: an HTTP/2 client that answers PINGs but never sends a request holds a connection permit indefinitely (hyper 1.8 has no h2 idle-connection timeout; `keep_alive_while_idle` only detects dead peers). Revisit if hyper adds one or an idle-reaper is wanted.

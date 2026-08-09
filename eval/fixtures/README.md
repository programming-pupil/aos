# Parity fixtures

These fixtures are intentionally small, inspectable sources for command-based AOS/Codex
comparison runs. The adapter request includes the selected `fixturePath`; an AOS command
adapter must upload or mount it through the authenticated workspace APIs rather than expose
the host path directly.

The HTTP adapter currently covers live chat/tool-loop cases. File, SQL, long-context, and
isolation cases require a command adapter that performs the matching authenticated setup.

# Development signing keys

The ZaminMC development frontend-signing keypair. The public half
is embedded in the engine's built-in trust store; the private half
signs test fixtures and development builds of frontend packages.

This key carries NO release authority: production signing keys are
rotated out-of-band before any signed release channel exists
(docs/api/FRONTEND_EXTENSION_SPEC.md §3.4). It is committed openly
so the engine's verification path is exercised end-to-end in CI.

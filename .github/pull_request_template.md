## Qué cambia

<una o dos líneas>

## Pruebas

- [ ] `cargo test -j 1 -p orus-api -p orus-app -p orus-server -p orus-shell`
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] Probado en la ventana (o explicado por qué no aplica)

## Backend

- [ ] Esta versión no toca backend.
- [ ] Esta versión toca migraciones, edge functions, MCP, autenticación, tokens,
      RLS o Realtime. Avisé a la persona responsable del backend y esperé su
      confirmación antes de publicar.

## Versión

- [ ] `Cargo.toml` (workspace) y `CHANGELOG.md` dicen la misma versión.
- [ ] La nota de release sigue `docs/nota-release.md`.

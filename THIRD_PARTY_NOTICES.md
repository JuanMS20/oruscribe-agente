# Avisos de terceros

Oruscribe Agente, © 2026 Oruscribe.

**Este producto no contiene código de goose ni de Syncra-agent.** Se escribió desde
cero en Rust. Las dependencias que viajan dentro del ejecutable se listan debajo,
con la versión que fija `Cargo.lock` y la licencia que declara cada paquete en
`cargo metadata --locked`. La lista se actualizó a mano el 2026-11-20; no hay un
paso automático que la regenere. Cubre las dependencias directas del ejecutable;
las transitivas quedan en `Cargo.lock`, que es la fuente completa de versiones.

## Rust standard library

MIT OR Apache-2.0. © The Rust Project Developers.
<https://www.rust-lang.org/policies/licenses>

## Dependencias directas

- **async-trait 0.1.92** — MIT OR Apache-2.0.
  <https://github.com/dtolnay/async-trait>
- **axum 0.8.9** — MIT. <https://github.com/tokio-rs/axum>
- **base64 0.23.1** — MIT OR Apache-2.0.
  <https://github.com/marshallpierce/rust-base64>
- **chrono 0.4.45** — MIT OR Apache-2.0.
  <https://github.com/chronotope/chrono>
- **ed25519-dalek 2.2.0** — BSD-3-Clause.
  <https://github.com/dalek-cryptography/curve25519-dalek/tree/main/ed25519-dalek>
- **futures-util 0.3.34** — MIT OR Apache-2.0.
  <https://github.com/rust-lang/futures-rs>
- **hex 0.4.3** — MIT OR Apache-2.0.
  <https://github.com/KokaKiwi/rust-hex>
- **hound 3.5.1** — Apache-2.0. <https://github.com/ruuda/hound>
- **png 0.18.1** — MIT OR Apache-2.0.
  <https://github.com/image-rs/image-png>
- **rand 0.8.8** — MIT OR Apache-2.0.
  <https://github.com/rust-random/rand>
- **regex 1.13.1** — MIT OR Apache-2.0.
  <https://github.com/rust-lang/regex>
- **reqwest 0.13.5** — MIT OR Apache-2.0.
  <https://github.com/seanmonstar/reqwest>
- **rmcp 3.9.0** — Apache-2.0.
  <https://github.com/modelcontextprotocol/rust-sdk/>
- **serde 1.0.229** — MIT OR Apache-2.0.
  <https://github.com/serde-rs/serde>
- **serde_json 1.0.154** — MIT OR Apache-2.0.
  <https://github.com/serde-rs/json>
- **sha2 0.10.9** — MIT OR Apache-2.0.
  <https://github.com/RustCrypto/hashes>
- **tao 0.37.2** — Apache-2.0. <https://github.com/tauri-apps/tao>
- **thiserror 2.0.21** — MIT OR Apache-2.0.
  <https://github.com/dtolnay/thiserror>
- **tokio 1.54.1** — MIT. <https://github.com/tokio-rs/tokio>
- **tokio-tungstenite 0.30.0** — MIT.
  <https://github.com/snapview/tokio-tungstenite>
- **tray-icon 0.26.1** — MIT OR Apache-2.0.
  <https://github.com/tauri-apps/tray-icon>
- **whisper-rs 0.16.0** — Unlicense (dominio público).
  <https://codeberg.org/tazz4843/whisper-rs>
- **windows-sys 0.61.2** — MIT OR Apache-2.0.
  <https://github.com/microsoft/windows-rs>
- **wry 0.57.2** — Apache-2.0 OR MIT. <https://github.com/tauri-apps/wry>

## Catálogo de proveedores de @earendil-works/pi-ai

MIT. © 2026 Mario Zechner. <https://github.com/earendil-works/pi>

`crates/orus-agent/src/catalog.rs` lista los proveedores con su nombre, la
dirección a la que hay que llamar y el protocolo que hablan. Esos datos salen del
catálogo que pi-ai publica en su paquete npm. No viaja código de pi-ai dentro del
ejecutable: solo esas tres columnas, y este arnés habla los protocolos por su
cuenta, en Rust.

## Motor de dictado local

`crates/orus-voice` enlaza whisper.cpp dentro del ejecutable a través de
`whisper-rs-sys 0.15.0`, que lo trae vendorizado.

whisper.cpp — MIT. © 2023-2024 The ggml authors.
<https://github.com/ggml-org/whisper.cpp>

Los pesos de los modelos (`ggml-base-q5_1.bin`, `ggml-small-q5_1.bin`) son de
OpenAI Whisper — MIT, © OpenAI — y se descargan en tiempo de ejecución desde
<https://huggingface.co/ggerganov/whisper.cpp>; no viajan en el ejecutable. El
catálogo de `crates/orus-voice/src/modelos.rs` fija su tamaño y su SHA-256, que
se verifica antes de usarlos.

## Tipografía Inter

SIL Open Font License 1.1. © 2016 The Inter Project Authors, Rasmus Andersson.
<https://github.com/rsms/inter>

Se distribuye embebida en `frontend/dist/fonts/inter.woff2`. No se descarga de
Google en tiempo de ejecución.

## Marca

El logotipo, el nombre Oruscribe y la interfaz son propiedad de Oruscribe. No hay
activos de terceros en la marca.

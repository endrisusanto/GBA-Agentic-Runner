# GBA Agentic Runner

Desktop runner berbasis Tauri untuk otomatisasi CTS, GTS, dan STS dari folder AUTO.

## Fitur

- Scan device ADB dan tampilkan metadata build.
- Mode run: `Cuci SMR`, `MR`, `SMR`, `SKU`, dan `STS`.
- Wi-Fi auto connect, retry args, timeout, `ro.xml`, live log, summary metrics, cancel process, dan open result folder.
- UI dark dashboard dengan running log di bawah workspace.

## Requirement

- Linux desktop.
- Node.js 20+ dan Rust stable.
- Dependency Tauri Linux: GTK/WebKitGTK, AppIndicator, librsvg, patchelf, rpm.
- Runtime Android tools: `adb`, `java`.
- Folder suite lokal: `CTS/`, `GTS/`, `STS/`, dan `Results/`.

## Development

```bash
npm install
npm run dev
```

Build frontend saja:

```bash
npm run web:build
```

Check Rust:

```bash
cargo check --manifest-path src-tauri/Cargo.toml
```

## Build Linux Lokal

Gunakan script:

```bash
./build-linux.sh
```

Output `.deb` dan `.rpm` akan dibuat di:

```text
src-tauri/target/release/bundle/
```

## Release Tag

Script release akan membaca tag semver terakhir, menaikkan versi, update versi di `package.json`, `package-lock.json`, `src-tauri/Cargo.toml`, dan `src-tauri/tauri.conf.json`, lalu membuat commit dan annotated tag.

Patch release lokal:

```bash
./scripts/release-next.sh patch
```

Minor atau major:

```bash
./scripts/release-next.sh minor
./scripts/release-next.sh major
```

Buat commit/tag lalu langsung push:

```bash
./scripts/release-next.sh patch --push
```

Saat tag `vX.Y.Z` dipush ke GitHub, workflow `.github/workflows/release.yml` otomatis build `.deb` dan `.rpm`, lalu upload ke GitHub Release.

## Catatan Repo

Folder suite Android dan hasil test tidak dimasukkan ke git karena ukurannya besar dan bersifat lokal:

- `CTS/`
- `GTS/`
- `STS/`
- `Results/`
- `KEY/`

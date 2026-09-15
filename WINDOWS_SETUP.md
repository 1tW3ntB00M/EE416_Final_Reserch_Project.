# Windows build setup (GDAL / odin-rs patch)

This branch makes `odin-gsp` build on Windows with vcpkg GDAL 3.12.x.

## 1. Layout

```
<ODIN>/
  odin-gsp/   <- this repo
  odin-rs/    <- sibling clone, required by the [patch] in Cargo.toml
  vcpkg/      <- vcpkg with gdal installed (x64-windows)
```

## 2. odin-rs sibling clone

```
cd <ODIN>
git clone https://github.com/ODIN-fire/odin-rs
```

In `<ODIN>/odin-rs/Cargo.toml`, drop the `bindgen` feature so
`gdal-sys` uses its prebuilt `3_12` bindings instead of regenerating
them (fresh MSVC bindgen emits `c_int` where `gdal 0.19` expects
`c_uint` -> 58 type errors):

```toml
gdal = { version = "0.19", features = ["array"] }
gdal-sys = { version = "0.12" }
```

`odin-gsp/Cargo.toml` already contains:

```toml
[patch."https://github.com/ODIN-fire/odin-rs"]
odin_actor = { path = "../odin-rs/odin_actor" }
odin_build = { path = "../odin-rs/odin_build" }
odin_action = { path = "../odin-rs/odin_action" }
odin_common = { path = "../odin-rs/odin_common" }
odin_server = { path = "../odin-rs/odin_server" }
odin_cesium = { path = "../odin-rs/odin_cesium" }
odin_openmeteo = { path = "../odin-rs/odin_openmeteo" }
```

## 3. GDAL native lib (vcpkg)

```
cd <ODIN>/vcpkg
vcpkg install gdal:x64-windows
```

Persist env (adjust `<ODIN>`; delete `GDAL_HOME` if you have a
GISInternals install — it hijacks linking):

```
setx GDAL_VERSION "3.12.4"
setx GDAL_INCLUDE_DIR "<ODIN>\vcpkg\installed\x64-windows\include"
setx GDAL_LIB_DIR "<ODIN>\vcpkg\installed\x64-windows\lib"
```

Delete user `GDAL_HOME` if present:

```
Remove-ItemProperty -Path HKCU:\Environment -Name GDAL_HOME -ErrorAction SilentlyContinue
```

Then **restart the terminal** (`$env:` edits are session-only) and
ensure the DLL is on PATH for build/run:

```
$env:PATH="<ODIN>\vcpkg\installed\x64-windows\bin;$env:PATH"
```

## 4. Build

```
cd <ODIN>\odin-gsp
cargo clean -p gdal-sys -p gdal
cargo build
```

## Notes

* Do NOT set `GDAL_VERSION=0.12.0` — that is the Rust crate version,
  not libgdal. Use the real library version (`3.12.4`).
* Do not build the whole `odin-rs` workspace on Windows: `gpshub`
  calls `socket2::Socket::set_reuse_port` (Unix-only, `E0599`).
  Build `odin-gsp` instead.

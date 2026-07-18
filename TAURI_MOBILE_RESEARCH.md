# Tauri 2 — Mobile & Tablet Build Research

> Research summary: top open-source Tauri 2 projects that ship to Android/iOS
> (phones + tablets) and the setup/config/plugins they use.

## 1. Does Tauri support tablets?

Yes. **Tauri v2** (stable since Aug 2024) is the first version with first-class
mobile support. Tablets run standard Android/iPadOS, so they use the same build
path as phones — no tablet-specific flag needed.

| Version | Mobile |
|---|---|
| Tauri v1 | Desktop only (no mobile) |
| Tauri v2 | Android + iOS (arm + x86-64 device targets) |

- Android: minSdk 24 (Android 7+; many apps target 30/Android 12+)
- iOS/iPadOS: 9+ (iPad supported)
- WebView: system Android System WebView / iOS WKWebView

## 2. How the UI adapts to mobile vs desktop

Tauri ships **no** UI framework — it is a webview shell. Adaptation is your web
code's job (same as any website):

- **Responsive CSS**: media queries (`@media (max-width: 768px)`), Flexbox/Grid,
  `clamp()`/`vw`, frameworks (Tailwind, UnoCSS, Bootstrap).
- **Platform detection (JS)**:
  ```js
  import { platform } from '@tauri-apps/plugin-os'
  const os = await platform() // 'android' | 'ios' | 'linux' | ...
  ```
  Compile-time flag in some projects: `process.env.__TAURI_MOBILE__`.
- **Native feel**: `env(safe-area-inset-*)` for notches/gesture bars; ≥44px touch
  targets; momentum scroll; swipe/gesture handlers.
- **Tauri hooks**: `plugin-device` (screen size, DPR), `Window` API (resize,
  fullscreen, safe-area), `plugin-shell` / deep links, notifications.

## 3. Top projects to learn setup/config/plugins from

| Project | Stars | Stack | Mobile | Why |
|---|---|---|---|---|
| **HuLa** | 7.4k | Rust + Vue3 + Vite7 + UnoCSS | Android 12+, iOS 9+, desktop | Best real production example: one codebase → desktop + phone + tablet |
| **quantum** | 529 | Tauri + SolidStart (TS) | iOS, Android, desktop | Scaffold template w/ tuned release Cargo profile |
| **vscode-android** | ~small | React + TS + Vite + Tailwind | Android | Cleanest concrete `tauri.conf.json` + `capabilities/android.json` |
| **bastion** | — | React (shared web) | iOS + Android | `__TAURI_MOBILE__` detection + per-platform `tauri.*.conf.json` |
| **two** | 11 | SvelteKit + Tauri | iOS, Android | Minimal Svelte mobile reference |
| **brenogonzaga/tauri-plugin-*** | 15–19 | Rust plugins | iOS + Android | Cross-platform mobile plugins: stt, tts, audio-recorder |

### How each adapts UI
- **HuLa**: separate `locales/*/mobile_*.json` + `public/Mobile/*.png`; one Vue app,
  responsive via UnoCSS breakpoints; ships `docs/android_startup_guide.md`.
- **bastion**: `if (process.env.__TAURI_MOBILE__)` swaps nav drawer ↔ sidebar;
  same React frontend for android/ios.
- **vscode-android**: Tailwind responsive + 44px touch targets + gestures + fullscreen.

## 4. Concrete setup you can copy (from `vscode-android`)

### `src-tauri/Cargo.toml`
```toml
[package]
name = "vscode-android"
version = "1.0.0"
edition = "2021"

[lib]
name = "vscode_android_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[dependencies]
tauri = { version = "^2", features = [] }
tauri-build = { version = "^2" }
tauri-plugin-fs = "^2"
tauri-plugin-http = "^2"
tauri-plugin-shell = "^2"
tauri-plugin-store = "^2"
tauri-plugin-websocket = "^2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.36", features = ["full"] }

[profile.release]
codegen-units = 1
lto = true
opt-level = "s"
strip = true
panic = "abort"   # from quantum's tuned release profile
```

### `tauri.conf.json`
```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "VSCode Android",
  "version": "1.0.0",
  "identifier": "com.vscode.android",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://localhost:1420",
    "beforeDevCommand": "npm run dev",
    "beforeBuildCommand": "npm run build"
  },
  "app": {
    "windows": [{ "title": "VSCode Android", "width": 1920, "height": 1080, "resizable": true }],
    "security": {
      "csp": "default-src 'self'; connect-src 'self' https://api.github.com wss://*; img-src 'self' data: https://*; script-src 'self' 'unsafe-inline' 'unsafe-eval'; style-src 'self' 'unsafe-inline'"
    }
  },
  "bundle": {
    "active": true,
    "icon": ["icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png", "icons/icon.icns", "icons/icon.ico"],
    "android": { "minSdkVersion": 24 }
  },
  "plugins": {
    "http": { "scope": ["https://api.github.com", "https://github.com", "wss://*"] },
    "websocket": { "scope": ["wss://*.github.dev", "wss://api.github.com"] },
    "store": { "path": "store.bin" }
  }
}
```

### `src-tauri/capabilities/android.json`
```json
{
  "identifier": "android-capability",
  "description": "Capability for Android platform",
  "windows": ["main"],
  "permissions": ["core:default", "fs:allow-read-text-file", "websocket:default"],
  "platforms": ["android"]
}
```
Mirror with `capabilities/ios.json` (`"platforms": ["ios"]`), and platform
overrides via `tauri.android.conf.json` / `tauri.ios.conf.json`.

## 5. Build & signing commands

```bash
# init mobile projects (generates src-tauri/gen/android|ios)
cargo tauri android init
cargo tauri ios init

# dev on device/simulator
cargo tauri android dev
cargo tauri ios dev --open

# release builds
cargo tauri android build            # unsigned APK
cargo tauri android build --apk      # APK
cargo tauri android build --aab      # AAB for Play Store
cargo tauri ios build
```

Android signing is configured in `tauri.conf.json` (`bundle.android.*`) or via
`src-tauri/gen/android/gradle.properties` + `build.gradle.kts` signingConfigs.

## 6. Recommendation

- **Fork as a production base** → **HuLa** (monorepo, `.cargo/config.toml`,
  `docs/android_startup_guide.md`, mobile locales, desktop+tablet+phone+iOS).
- **Minimal clean config reference** → copy `tauri.conf.json` +
  `capabilities/android.json` + `Cargo.toml` from **vscode-android**.
- **Cross-platform mobile features** → **brenogonzaga** plugins (stt / tts /
  audio-recorder).
- **Tuned release binary size** → **quantum**'s `panic="abort"`, `lto`,
  `opt-level="s"`, `strip`, `codegen-units=1` profile.

## 7. Sources

- https://github.com/HuLaSpark/HuLa
- https://github.com/atilafassina/quantum
- https://github.com/swadhinbiswas/vscode-android
- https://github.com/Calmingstorm/bastion
- https://github.com/minosiants/two
- https://github.com/brenogonzaga/tauri-plugin-stt (et al.)
- https://v2.tauri.app/reference/config/

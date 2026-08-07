# Threadknot Mobile — agent notes

Expo SDK 57 (React Native 0.86, React 19). APIs have changed across SDK
versions — check https://docs.expo.dev/versions/v57.0.0/ before writing
Expo-API code from memory.

- UI components in `src/components/ui/` are vendored react-native-reusables
  (NativeWind flavor) — edit them freely, they are code-owned.
- Styling is NativeWind 4 (tailwind.config.js, src/global.css). Threadknot is
  dark-only; theme tokens mirror threadknot/src/styles.css.
- Server/push/auth contract lives in ../docs/MOBILE.md; the Rust side is
  ../src-tauri/src/{mobile.rs,push.rs,server.rs}.
- Verify gate: `npm run typecheck`, `npx expo-doctor`, and
  `npx expo export --platform android` must all pass.

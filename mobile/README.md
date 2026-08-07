# Threadknot Mobile

Native Expo companion for [Threadknot](../README.md): biometric-locked shell around
the Threadknot web UI with multi-server switching and push notifications that
deep-link to the thread needing attention.

- **Docs / architecture:** [`../docs/MOBILE.md`](../docs/MOBILE.md)
- **EAS project:** `@servicestorm/threadknot-mobile`

```bash
npm install
npm run typecheck                                  # tsc --noEmit
npx eas-cli build --profile development --platform android   # dev build (needed for biometrics + push)
npx expo start --dev-client
```

Pairing: in the desktop app open Settings, copy the LAN URL (with `?token=…`),
paste it into "Add server" here. The master token is exchanged for a revocable
device credential and never stored on the phone.

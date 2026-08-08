import { Alert, Platform, Share } from 'react-native';
import { sameOrigin } from './api';

/** Server endpoints that serve raw file bytes. A main-frame navigation to one
 * of these must never happen inside the WebView — the SPA has no browser
 * chrome, so the user would be stranded on a full-screen file with no way
 * back. We intercept and download natively instead. */
const FILE_PATHS = ['/artifact-file', '/file', '/attachment'];

export function isFileUrl(url: string, baseUrl: string): boolean {
  // Origin equality, not a string prefix: `baseUrl` carries no trailing slash,
  // so a prefix test also matches `<baseUrl>.attacker.example`.
  if (!sameOrigin(url, baseUrl)) return false;
  try {
    return FILE_PATHS.includes(new URL(url).pathname);
  } catch {
    return false;
  }
}

/** Best-effort filename from the URL (`/file?path=…` carries one); otherwise
 * let the server's Content-Disposition name the download. */
function guessName(url: string): string | null {
  try {
    const path = new URL(url).searchParams.get('path');
    const base = path?.split('/').pop()?.trim();
    return base && base.length > 0 ? base : null;
  } catch {
    return null;
  }
}

/** Download the file to cache and hand it to the system share sheet (save to
 * Files, AirDrop, open in app, …). Falls back to the OS share dialog.
 *
 * `credential` is this device's SecureStore token, sent as an `Authorization`
 * header. Expo FileSystem is a separate network stack from the WebView, so it
 * inherits neither the WebView's cookie jar nor the credential injected into
 * page scope — without the header these downloads simply 401 once the server
 * stops accepting credentials in URLs (SEC-006).
 *
 * This is also why the header, and not a URL parameter, is the only shape that
 * works against a relay origin at all: the strict ingress answers any
 * credential-bearing query key with a 400, valid or not. The bearer is accepted
 * there for exactly this case — a native client with a keychain, which a browser
 * is not.
 *
 * There is deliberately no browser fallback. Handing a protected URL to the
 * system browser sends it to an app with no credential at all: on a good day
 * that is a confusing 401, and on a bad one it is a protected URL sitting in
 * another app's history and sync. A failed download says so instead. */
export async function downloadAndShare(url: string, credential?: string): Promise<void> {
  let fileUri: string;
  try {
    const { Directory, File, Paths } = await import('expo-file-system');
    // Unique directory per download: no name collisions, and the server's
    // suggested filename (Content-Disposition) is preserved.
    const dir = new Directory(Paths.cache, 'threadknot-downloads', Date.now().toString(36));
    dir.create({ intermediates: true, idempotent: true });
    const name = guessName(url);
    const file = await File.downloadFileAsync(url, name ? new File(dir, name) : dir, {
      headers: credential ? { Authorization: `Bearer ${credential}` } : undefined,
    });
    fileUri = file.uri;
  } catch (e) {
    Alert.alert('Download failed', e instanceof Error ? e.message : String(e));
    return;
  }

  try {
    const Sharing = await import('expo-sharing');
    if (await Sharing.isAvailableAsync()) {
      await Sharing.shareAsync(fileUri);
      return;
    }
  } catch {
    // expo-sharing not in this build — fall through.
  }
  if (Platform.OS === 'ios') {
    await Share.share({ url: fileUri }).catch(() => undefined);
    return;
  }
  Alert.alert('Downloaded', 'Saved to the app cache, but no share handler is available.');
}

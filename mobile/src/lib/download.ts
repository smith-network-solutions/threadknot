import { Alert, Linking, Platform, Share } from 'react-native';

/** Server endpoints that serve raw file bytes. A main-frame navigation to one
 * of these must never happen inside the WebView — the SPA has no browser
 * chrome, so the user would be stranded on a full-screen file with no way
 * back. We intercept and download natively instead. */
const FILE_PATHS = ['/artifact-file', '/file', '/attachment'];

export function isFileUrl(url: string, baseUrl: string): boolean {
  if (!url.startsWith(baseUrl)) return false;
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
 * Files, AirDrop, open in app, …). Falls back to the OS share dialog and, as
 * a last resort, the system browser. */
export async function downloadAndShare(url: string): Promise<void> {
  let fileUri: string;
  try {
    const { Directory, File, Paths } = await import('expo-file-system');
    // Unique directory per download: no name collisions, and the server's
    // suggested filename (Content-Disposition) is preserved.
    const dir = new Directory(Paths.cache, 'threadknot-downloads', Date.now().toString(36));
    dir.create({ intermediates: true, idempotent: true });
    const name = guessName(url);
    const file = await File.downloadFileAsync(url, name ? new File(dir, name) : dir);
    fileUri = file.uri;
  } catch (e) {
    // Native download unavailable/failed — let the system browser handle it.
    try {
      await Linking.openURL(url);
    } catch {
      Alert.alert('Download failed', e instanceof Error ? e.message : String(e));
    }
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

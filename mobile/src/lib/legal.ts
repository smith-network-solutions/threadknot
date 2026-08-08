import * as WebBrowser from 'expo-web-browser';
import { Alert, Linking } from 'react-native';

/**
 * The canonical copies of everything the App Store review needs to reach.
 *
 * The screens under `app/settings/` carry the text in-app as well, so a
 * reviewer never depends on the site being up — but App Store Connect wants a
 * URL, and these are those URLs.
 */
export const LEGAL = {
  privacy: 'https://threadknot.ai/privacy',
  terms: 'https://threadknot.ai/terms',
  support: 'https://threadknot.ai/support',
  home: 'https://threadknot.ai',
  supportEmail: 'support@threadknot.ai',
  /** Where a deletion request lands once the backend for it exists. */
  deleteAccount: 'https://threadknot.ai/account/delete',
} as const;

/** Open a URL in the in-app browser, falling back to the system one. */
export async function openExternal(url: string): Promise<void> {
  try {
    await WebBrowser.openBrowserAsync(url, {
      // Match the console rather than flashing a white sheet on a dark app.
      toolbarColor: '#0b0d12',
      controlsColor: '#d9a35c',
    });
  } catch {
    const ok = await Linking.canOpenURL(url).catch(() => false);
    if (ok) await Linking.openURL(url);
    else Alert.alert('Could not open link', url);
  }
}

export function mailSupport(subject: string): void {
  const url = `mailto:${LEGAL.supportEmail}?subject=${encodeURIComponent(subject)}`;
  void Linking.openURL(url).catch(() =>
    Alert.alert('No mail app', `Write to ${LEGAL.supportEmail}.`)
  );
}

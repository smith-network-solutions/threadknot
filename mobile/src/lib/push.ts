import Constants from 'expo-constants';
import * as Device from 'expo-device';
import * as Notifications from 'expo-notifications';
import { Platform } from 'react-native';
import { updatePushRegistration } from './api';
import type { ServerProfile } from './types';

/** Android channel id — must match the `channelId` the Rust server sends. */
export const CHANNEL_ID = 'threadknot';

export function deviceLabel(): string {
  return Device.deviceName ?? Device.modelName ?? `${Platform.OS} device`;
}

async function ensureChannel(): Promise<void> {
  if (Platform.OS !== 'android') return;
  await Notifications.setNotificationChannelAsync(CHANNEL_ID, {
    name: 'Agent activity',
    importance: Notifications.AndroidImportance.MAX,
    vibrationPattern: [0, 90, 60, 90],
    lightColor: '#d9a35c',
  });
}

/** Ask for permission (first call) and fetch this install's Expo push token.
 * Returns null on simulators, denied permission, or missing EAS project id. */
export async function getPushToken(): Promise<string | null> {
  if (!Device.isDevice) return null;
  let { status } = await Notifications.getPermissionsAsync();
  if (status !== 'granted') {
    ({ status } = await Notifications.requestPermissionsAsync());
  }
  if (status !== 'granted') return null;
  await ensureChannel();
  const projectId: string | undefined = Constants.expoConfig?.extra?.eas?.projectId;
  if (!projectId) return null;
  try {
    const token = await Notifications.getExpoPushTokenAsync({ projectId });
    return token.data;
  } catch (e) {
    console.warn('push token fetch failed', e);
    return null;
  }
}

/** Push our current token + preferences to one server. Quiet best-effort:
 * callers decide whether failures should surface. */
export async function registerPushForServer(
  profile: ServerProfile,
  credential: string
): Promise<void> {
  const expoPushToken = await getPushToken();
  await updatePushRegistration(profile.baseUrl, credential, {
    ...(expoPushToken ? { expoPushToken } : {}),
    notificationsEnabled: profile.notificationsEnabled,
    deviceName: deviceLabel(),
  });
}

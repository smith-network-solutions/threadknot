import * as LocalAuthentication from 'expo-local-authentication';
import * as React from 'react';
import { AppState, AppStateStatus } from 'react-native';

export type LockStatus =
  /** Probing device security support. */
  | 'checking'
  /** Waiting on biometric/passcode auth. Content must stay covered. */
  | 'locked'
  | 'unlocked'
  /** No device security enrolled at all — refuse to expose servers. */
  | 'unavailable';

export interface LockValue {
  status: LockStatus;
  /** App is inactive/backgrounded — cover content so the app-switcher
   * snapshot can't leak agent conversations. */
  obscured: boolean;
  unlock(): Promise<void>;
  recheck(): Promise<void>;
}

const LockContext = React.createContext<LockValue | null>(null);

/** Re-lock only after this long in the background. Quick app switches (check
 * a message, come back) shouldn't demand Face ID every single time. */
const RELOCK_AFTER_MS = 5 * 60 * 1000;

export function useLock(): LockValue {
  const v = React.useContext(LockContext);
  if (!v) throw new Error('useLock outside LockProvider');
  return v;
}

export function LockProvider({ children }: { children: React.ReactNode }) {
  const [status, setStatus] = React.useState<LockStatus>('checking');
  const [obscured, setObscured] = React.useState(false);
  const statusRef = React.useRef(status);
  statusRef.current = status;
  const authBusy = React.useRef(false);

  const unlock = React.useCallback(async () => {
    if (authBusy.current || statusRef.current === 'unlocked') return;
    authBusy.current = true;
    try {
      const res = await LocalAuthentication.authenticateAsync({
        promptMessage: 'Unlock Threadknot',
        cancelLabel: 'Cancel',
      });
      if (res.success) setStatus('unlocked');
    } finally {
      authBusy.current = false;
    }
  }, []);

  const recheck = React.useCallback(async () => {
    const level = await LocalAuthentication.getEnrolledLevelAsync().catch(
      () => LocalAuthentication.SecurityLevel.NONE
    );
    if (level === LocalAuthentication.SecurityLevel.NONE) {
      setStatus('unavailable');
      return;
    }
    setStatus((s) => (s === 'unlocked' ? s : 'locked'));
    void unlock();
  }, [unlock]);

  // When the app last went to full background while unlocked. The re-lock
  // decision happens on return, so short hops away stay friction-free.
  const backgroundedAt = React.useRef<number | null>(null);

  React.useEffect(() => {
    void recheck();
    const sub = AppState.addEventListener('change', (next: AppStateStatus) => {
      if (next === 'active') {
        setObscured(false);
        const away = backgroundedAt.current;
        backgroundedAt.current = null;
        if (
          statusRef.current === 'unlocked' &&
          away != null &&
          Date.now() - away > RELOCK_AFTER_MS
        ) {
          setStatus('locked');
          statusRef.current = 'locked'; // unlock() reads the ref before React commits
          void unlock();
          return;
        }
        if (statusRef.current === 'locked') void unlock();
        if (statusRef.current === 'unavailable') void recheck();
      } else {
        setObscured(true);
        // Note the moment of a full background (not a mere permission dialog /
        // app-switcher peek). Cold starts always lock regardless.
        if (next === 'background' && statusRef.current === 'unlocked' && backgroundedAt.current == null) {
          backgroundedAt.current = Date.now();
        }
      }
    });
    return () => sub.remove();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const value = React.useMemo<LockValue>(
    () => ({ status, obscured, unlock, recheck }),
    [status, obscured, unlock, recheck]
  );

  return <LockContext.Provider value={value}>{children}</LockContext.Provider>;
}

/** Pick an image, crop it interactively, and turn it into a compact data URL.
 * Data URLs keep the same image usable in Tauri and token-gated LAN clients. */

export interface CropSpec {
  /** Output square edge in px. */
  size: number;
  /** Canvas encode target (webp falls back to png on engines without it). */
  mime: string;
  quality: number;
  /** Maximum encoded image bytes accepted by the destination. */
  maxBytes?: number;
  /** Opaque backdrop painted first (required for jpeg, which has no alpha). */
  background: string;
}

interface PickOptions {
  /** Output square edge in px. */
  size?: number;
  /** Canvas encode target (webp falls back to png on engines without it). */
  mimeType?: string;
  quality?: number;
  /** Opaque backdrop painted first (required for jpeg, which has no alpha). */
  background?: string;
}

/** One pending interactive crop, handed to the mounted <AvatarCropHost/>. */
export interface CropRequest {
  /** Object URL for a pre-chosen file (revoked when resolve fires), or
   *  null → the modal opens on its "click here to upload" panel first. */
  src: string | null;
  spec: CropSpec;
  resolve: (dataUrl: string | null) => void;
}

/** The mounted <AvatarCropHost/> (see components/AvatarCropModal). Null →
 *  no UI available; cropImageFlow falls back to an automatic center crop. */
let cropHost: ((req: CropRequest) => void) | null = null;

export function registerCropHost(host: ((req: CropRequest) => void) | null) {
  cropHost = host;
}

/** Crop `file` through the modal (drag/zoom, circular mask) and resolve with
 * the encoded data URL, or null when the user cancels. Programmatic bypass:
 * with no crop host mounted this degrades to the old automatic center crop. */
export function cropImageFlow(file: File, spec: CropSpec): Promise<string | null> {
  const host = cropHost;
  if (!host) return autoCenterCrop(file, spec);
  const src = URL.createObjectURL(file);
  return new Promise((resolve) => {
    host({
      src,
      spec,
      resolve: (url) => {
        URL.revokeObjectURL(src);
        resolve(url);
      },
    });
  });
}

/** Open the modal on its upload panel first (no file chosen yet): dropzone →
 *  crop → save. Resolves with the encoded data URL, or null on cancel. */
export function uploadImageFlow(spec: CropSpec): Promise<string | null> {
  const host = cropHost;
  if (!host) return Promise.resolve(null);
  return new Promise((resolve) => host({ src: null, spec, resolve }));
}

export function pickSidebarImage(opts: PickOptions = {}): Promise<string | null> {
  const {
    size = 256,
    mimeType = "image/webp",
    quality = 0.86,
    background = "#10141d",
  } = opts;
  // Preferred path: the modal opens FIRST ("click here to upload"), then the
  // scaler, then save: one consistent flow for every "set image" button.
  if (cropHost) {
    return uploadImageFlow({ size, mime: mimeType, quality, background });
  }
  // Headless fallback (no crop host mounted): native file dialog + auto crop.
  return new Promise((resolve, reject) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/png,image/jpeg,image/webp,image/gif";
    input.onchange = () => {
      const file = input.files?.[0];
      if (!file) {
        resolve(null);
        return;
      }
      if (file.size > 10 * 1024 * 1024) {
        reject(new Error("Choose an image smaller than 10 MB."));
        return;
      }
      resolve(cropImageFlow(file, { size, mime: mimeType, quality, background }));
    };
    // Chromium/WebKit fire "cancel" when the file dialog is dismissed.
    input.addEventListener("cancel", () => resolve(null));
    input.click();
  });
}

/** Small avatar variant for machines + hermes agents: 96px jpeg lands a few
 * KB, comfortably under the server's 64 KB appearance-image limit. */
export const AVATAR_CROP_SPEC: CropSpec = {
  size: 96,
  mime: "image/jpeg",
  quality: 0.8,
  maxBytes: 64 * 1024,
  background: "#10141d",
};

export function pickAvatarImage(): Promise<string | null> {
  return pickSidebarImage({
    size: AVATAR_CROP_SPEC.size,
    mimeType: AVATAR_CROP_SPEC.mime,
    quality: AVATAR_CROP_SPEC.quality,
    background: AVATAR_CROP_SPEC.background,
  });
}

function loadImageFile(file: File): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onerror = () => {
      URL.revokeObjectURL(url);
      reject(new Error("That file is not a readable image."));
    };
    img.onload = () => {
      URL.revokeObjectURL(url);
      resolve(img);
    };
    img.src = url;
  });
}

/** Headless fallback: cover-fit center crop, no user interaction. */
async function autoCenterCrop(file: File, spec: CropSpec): Promise<string> {
  const source = await loadImageFile(file);
  const canvas = document.createElement("canvas");
  canvas.width = spec.size;
  canvas.height = spec.size;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Image processing is not available.");
  ctx.fillStyle = spec.background;
  ctx.fillRect(0, 0, spec.size, spec.size);
  const scale = Math.max(
    spec.size / source.naturalWidth,
    spec.size / source.naturalHeight,
  );
  const width = source.naturalWidth * scale;
  const height = source.naturalHeight * scale;
  ctx.drawImage(source, (spec.size - width) / 2, (spec.size - height) / 2, width, height);
  return canvas.toDataURL(spec.mime, spec.quality);
}

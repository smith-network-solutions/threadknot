const IMAGE_EXTENSIONS = new Set([
  "avif",
  "bmp",
  "gif",
  "heic",
  "heif",
  "ico",
  "jpeg",
  "jpg",
  "png",
  "svg",
  "tif",
  "tiff",
  "webp",
]);

/** MIME values are not reliable for file-manager drops (ICO often arrives as
 * application/octet-stream), so the name is a deliberate second signal. */
export function isImageAttachment(name: string, mimeType: string): boolean {
  const mime = mimeType.trim().toLowerCase();
  if (mime.startsWith("image/")) return true;
  const extension = name.toLowerCase().split(".").pop() ?? "";
  return IMAGE_EXTENSIONS.has(extension);
}

/** Give extension-only image files a useful type before they cross the wire. */
export function normalizedAttachmentMime(name: string, mimeType: string): string {
  const mime = mimeType.trim().toLowerCase();
  if (mime === "image/svg") return "image/svg+xml";
  const extension = name.toLowerCase().split(".").pop() ?? "";
  const byExtension: Record<string, string> = {
    avif: "image/avif",
    bmp: "image/bmp",
    gif: "image/gif",
    heic: "image/heic",
    heif: "image/heif",
    ico: "image/x-icon",
    jpeg: "image/jpeg",
    jpg: "image/jpeg",
    png: "image/png",
    svg: "image/svg+xml",
    tif: "image/tiff",
    tiff: "image/tiff",
    webp: "image/webp",
  };
  // Some file managers report ICO as application/ico and JPEG as image/jpg.
  // Let a known image extension repair those non-standard MIME values.
  if (byExtension[extension] && (!mime || mime === "application/octet-stream" || !mime.startsWith("image/"))) {
    return byExtension[extension];
  }
  if (mime === "image/jpg") return "image/jpeg";
  return byExtension[extension] ?? (mimeType || "application/octet-stream");
}

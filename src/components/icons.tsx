import type { ReactNode } from "react";

interface IconProps {
  size?: number;
  className?: string;
}

function svg(
  path: ReactNode,
  { size = 16, className }: IconProps,
  viewBox = "0 0 24 24",
) {
  return (
    <svg
      width={size}
      height={size}
      viewBox={viewBox}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden="true"
    >
      {path}
    </svg>
  );
}

export const AnchorIcon = (p: IconProps) =>
  svg(
    <>
      <circle cx="12" cy="5" r="2.6" />
      <path d="M12 7.6V21" />
      <path d="M12 21c-4.4 0-7.6-3.1-8-7.2M12 21c4.4 0 7.6-3.1 8-7.2" />
      <path d="M4 13.8l-1.7-.9M4 13.8l.5 1.9M20 13.8l1.7-.9M20 13.8l-.5 1.9" />
      <path d="M8.6 10h6.8" />
    </>,
    p,
  );

export const PlusIcon = (p: IconProps) => svg(<path d="M12 5v14M5 12h14" />, p);

/** Two stacked rails — the universal "grab here to reorder" handle. */
export const GripIcon = (p: IconProps) =>
  svg(<path d="M5 9.5h14M5 14.5h14" />, p);

/** Box with an arrow escaping the corner — "open in its own window". */
export const PopoutIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M11 5H6.5A2.5 2.5 0 0 0 4 7.5v10A2.5 2.5 0 0 0 6.5 20h10a2.5 2.5 0 0 0 2.5-2.5V13" />
      <path d="M14 4h6v6M20 4l-8 8" />
    </>,
    p,
  );

// Classic funnel — opens the sidebar view/filter popover.
export const FilterIcon = (p: IconProps) =>
  svg(<path d="M4 5h16l-6.4 7.6V19l-3.2-2v-4.4L4 5z" />, p);

export const SearchIcon = (p: IconProps) =>
  svg(
    <>
      <circle cx="11" cy="11" r="6.5" />
      <path d="M20 20l-4.2-4.2" />
    </>,
    p,
  );

export const GearIcon = (p: IconProps) =>
  svg(
    <>
      <circle cx="12" cy="12" r="3.2" />
      <path d="M19.4 15a1.6 1.6 0 0 0 .32 1.77l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.6 1.6 0 0 0-1.77-.32 1.6 1.6 0 0 0-.97 1.47V21a2 2 0 1 1-4 0v-.09a1.6 1.6 0 0 0-1-1.47 1.6 1.6 0 0 0-1.77.32l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.6 1.6 0 0 0 .32-1.77 1.6 1.6 0 0 0-1.47-.97H3a2 2 0 1 1 0-4h.09a1.6 1.6 0 0 0 1.47-1 1.6 1.6 0 0 0-.32-1.77l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.6 1.6 0 0 0 1.77.32h.08a1.6 1.6 0 0 0 .97-1.47V3a2 2 0 1 1 4 0v.09a1.6 1.6 0 0 0 .97 1.47 1.6 1.6 0 0 0 1.77-.32l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.6 1.6 0 0 0-.32 1.77v.08a1.6 1.6 0 0 0 1.47.97H21a2 2 0 1 1 0 4h-.09a1.6 1.6 0 0 0-1.47.97z" />
    </>,
    p,
  );

export const ChevronIcon = (p: IconProps & { open?: boolean }) =>
  svg(<path d={p.open ? "M6 9l6 6 6-6" : "M9 6l6 6-6 6"} />, p);

export const MenuIcon = (p: IconProps) =>
  svg(<path d="M4 6h16M4 12h16M4 18h16" />, p);

export const ArrowUpIcon = (p: IconProps) =>
  svg(<path d="M12 19V5M5.5 11.5L12 5l6.5 6.5" />, p);

export const ArrowDownIcon = (p: IconProps) =>
  svg(<path d="M12 5v14M5.5 12.5L12 19l6.5-6.5" />, p);

export const StopIcon = (p: IconProps) =>
  svg(<rect x="6.5" y="6.5" width="11" height="11" rx="2" fill="currentColor" stroke="none" />, p);

export const CopyIcon = (p: IconProps) =>
  svg(
    <>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M5 15V5a1.5 1.5 0 0 1 1.5-1.5H15" />
    </>,
    p,
  );

export const DownloadIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M12 4v11M7.5 10.5L12 15l4.5-4.5" />
      <path d="M5 19.5h14" />
    </>,
    p,
  );

export const XIcon = (p: IconProps) => svg(<path d="M6 6l12 12M18 6L6 18" />, p);

export const RefreshIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M20 11a8 8 0 1 0-.6 4" />
      <path d="M20 4.5V11h-6" />
    </>,
    p,
  );

export const PaperclipIcon = (p: IconProps) =>
  svg(
    <path d="M21 11.5l-8.6 8.6a5 5 0 0 1-7.1-7.1l8.6-8.6a3.3 3.3 0 0 1 4.7 4.7l-8.5 8.5a1.7 1.7 0 0 1-2.4-2.4l7.9-7.9" />,
    p,
  );

export const MicIcon = (p: IconProps) =>
  svg(
    <>
      <rect x="9" y="2.5" width="6" height="11.5" rx="3" />
      <path d="M5.5 11a6.5 6.5 0 0 0 13 0" />
      <path d="M12 17.5V21" />
    </>,
    p,
  );

export const TrashIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M4 7h16M9.5 7V4.8A1 1 0 0 1 10.5 4h3a1 1 0 0 1 1 1V7" />
      <path d="M6.5 7l.8 12.2a1.6 1.6 0 0 0 1.6 1.5h6.2a1.6 1.6 0 0 0 1.6-1.5L17.5 7" />
      <path d="M10 11v6M14 11v6" />
    </>,
    p,
  );

export const PencilIcon = (p: IconProps) =>
  svg(<path d="M4 20l.9-3.7L16.4 4.8a2 2 0 0 1 2.8 2.8L7.7 19.1 4 20z" />, p);

// Vertical 3-dot kebab — opens the per-thread actions menu.
export const MoreIcon = (p: IconProps) =>
  svg(
    <>
      <circle cx="12" cy="5" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" />
      <circle cx="12" cy="19" r="1.4" fill="currentColor" stroke="none" />
    </>,
    p,
  );

// Box with a down-arrow — archive (export + remove) action.
export const ArchiveIcon = (p: IconProps) =>
  svg(
    <>
      <rect x="3.5" y="4.5" width="17" height="4" rx="1" />
      <path d="M5 8.5v9A1.5 1.5 0 0 0 6.5 19h11a1.5 1.5 0 0 0 1.5-1.5v-9" />
      <path d="M12 11v4.2M9.8 13.2 12 15.4l2.2-2.2" />
    </>,
    p,
  );

export const FolderIcon = (p: IconProps) =>
  svg(
    <path d="M3.5 6.5A1.5 1.5 0 0 1 5 5h4.2l2 2.5H19a1.5 1.5 0 0 1 1.5 1.5v8.5A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5v-11z" />,
    p,
  );

export const FolderUpIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M3.5 6.5A1.5 1.5 0 0 1 5 5h4.2l2 2.5H19a1.5 1.5 0 0 1 1.5 1.5v8.5A1.5 1.5 0 0 1 19 19H5a1.5 1.5 0 0 1-1.5-1.5v-11z" />
      <path d="M12 16v-4.5M9.8 13.5L12 11.3l2.2 2.2" />
    </>,
    p,
  );

export const TerminalIcon = (p: IconProps) =>
  svg(<path d="M4.5 6.5l5 5.5-5 5.5M12.5 18.5H20" />, p);

export const FileIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M6 3.5h7.5L18.5 9v11a1.5 1.5 0 0 1-1.5 1.5H6A1.5 1.5 0 0 1 4.5 20V5A1.5 1.5 0 0 1 6 3.5z" />
      <path d="M13.5 3.5V9h5" />
    </>,
    p,
  );

export const GlobeIcon = (p: IconProps) =>
  svg(
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M3.5 12h17M12 3.5c2.5 2.4 3.8 5.3 3.8 8.5s-1.3 6.1-3.8 8.5c-2.5-2.4-3.8-5.3-3.8-8.5S9.5 5.9 12 3.5z" />
    </>,
    p,
  );

// Two panes side-by-side (side layout) — used for the workspace orientation toggle.
export const LayoutSideIcon = (p: IconProps) =>
  svg(
    <>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M14 4v16" />
    </>,
    p,
  );

// Two panes stacked (stacked layout) — used for the workspace orientation toggle.
export const LayoutStackedIcon = (p: IconProps) =>
  svg(
    <>
      <rect x="3" y="4" width="18" height="16" rx="2" />
      <path d="M3 14h18" />
    </>,
    p,
  );

export const WrenchIcon = (p: IconProps) =>
  svg(
    <path d="M14.2 6.3a4.3 4.3 0 0 1 5.6-5L16.6 4.5l2.9 2.9 3.2-3.2a4.3 4.3 0 0 1-5 5.6L9 18.5A2.1 2.1 0 1 1 5.5 15l8.7-8.7z" transform="scale(0.85) translate(2 2)" />,
    p,
  );

export const CheckIcon = (p: IconProps) => svg(<path d="M4.5 12.5l5 5L19.5 7" />, p);

// Curved arrow doubling back — "pull this back out of the settled shelf".
export const UndoIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M4 9h10a5.5 5.5 0 0 1 0 11h-6" />
      <path d="M8 4.5L3.5 9 8 13.5" />
    </>,
    p,
  );

// Five-point star: the favorite toggle. Outline by default; pass `filled` for
// the starred look (fills with currentColor so the brass tint carries through).
export const StarIcon = (p: IconProps & { filled?: boolean }) =>
  svg(
    <path
      d="M12 3.6l2.6 5.28 5.82.85-4.21 4.11.99 5.8L12 16.9l-5.2 2.74.99-5.8-4.21-4.11 5.82-.85L12 3.6z"
      fill={p.filled ? "currentColor" : "none"}
    />,
    p,
  );

// Notification bell for the per-workspace subscription toggle; `muted` adds
// the struck-through slash for "this workspace stays quiet on this device".
export const BellIcon = (p: IconProps & { muted?: boolean }) =>
  svg(
    <>
      <path d="M6 9a6 6 0 0 1 12 0c0 4 1.4 5.4 2 6.2H4c.6-.8 2-2.2 2-6.2z" />
      <path d="M10 18.6a2.2 2.2 0 0 0 4 0" />
      {p.muted && <path d="M4.2 4.2l15.6 15.6" />}
    </>,
    p,
  );

// Open eye for "bring this project back", struck through for "put it away".
// The `off` slash reuses the BellIcon convention so the two menus read alike.
export const EyeIcon = (p: IconProps & { off?: boolean }) =>
  svg(
    <>
      <path d="M2.5 12S6 5.8 12 5.8 21.5 12 21.5 12 18 18.2 12 18.2 2.5 12 2.5 12z" />
      <circle cx="12" cy="12" r="2.9" />
      {p.off && <path d="M4.2 4.2l15.6 15.6" />}
    </>,
    p,
  );

export const ClockIcon = (p: IconProps) =>
  svg(
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3.2 2" />
    </>,
    p,
  );

export const PlayIcon = (p: IconProps) =>
  svg(<path d="M8 5.5v13l10-6.5-10-6.5z" fill="currentColor" stroke="none" />, p);

export const ShieldIcon = (p: IconProps) =>
  svg(
    <path d="M12 3l7 2.8v5.4c0 4.5-3 8.1-7 9.8-4-1.7-7-5.3-7-9.8V5.8L12 3z" />,
    p,
  );

export const DiffIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M7 4v10M4 7h6" />
      <path d="M17 20V10M14 17h6" />
      <path d="M7 17.5a2.5 2.5 0 1 0 0 .01M17 6.5a2.5 2.5 0 1 0 0 .01" />
    </>,
    p,
  );

export const GitBranchIcon = (p: IconProps) =>
  svg(
    <>
      <path d="M6 3v12" />
      <circle cx="18" cy="6" r="3" />
      <circle cx="6" cy="18" r="3" />
      <path d="M18 9a9 9 0 0 1-9 9" />
    </>,
    p,
  );

export const UploadIcon = (p: IconProps) =>
  svg(<path d="M12 17V5M6 11l6-6 6 6M5 20h14" />, p);

export const DownloadCloudIcon = (p: IconProps) =>
  svg(<path d="M12 5v12M6 11l6 6 6-6M5 20h14" />, p);

// ---- provider brand marks ----------------------------------------------
// Official single-path logos (source: simpleicons.org — "Claude" / "OpenAI").
// These are fill icons: they honor `color` via fill="currentColor".

const CLAUDE_PATH =
  "m4.7144 15.9555 4.7174-2.6471.079-.2307-.079-.1275h-.2307l-.7893-.0486-2.6956-.0729-2.3375-.0971-2.2646-.1214-.5707-.1215-.5343-.7042.0546-.3522.4797-.3218.686.0608 1.5179.1032 2.2767.1578 1.6514.0972 2.4468.255h.3886l.0546-.1579-.1336-.0971-.1032-.0972L6.973 9.8356l-2.55-1.6879-1.3356-.9714-.7225-.4918-.3643-.4614-.1578-1.0078.6557-.7225.8803.0607.2246.0607.8925.686 1.9064 1.4754 2.4893 1.8336.3643.3035.1457-.1032.0182-.0728-.164-.2733-1.3539-2.4467-1.445-2.4893-.6435-1.032-.17-.6194c-.0607-.255-.1032-.4674-.1032-.7285L6.287.1335 6.6997 0l.9957.1336.419.3642.6192 1.4147 1.0018 2.2282 1.5543 3.0296.4553.8985.2429.8318.091.255h.1579v-.1457l.1275-1.706.2368-2.0947.2307-2.6957.0789-.7589.3764-.9107.7468-.4918.5828.2793.4797.686-.0668.4433-.2853 1.8517-.5586 2.9021-.3643 1.9429h.2125l.2429-.2429.9835-1.3053 1.6514-2.0643.7286-.8196.85-.9046.5464-.4311h1.0321l.759 1.1293-.34 1.1657-1.0625 1.3478-.8804 1.1414-1.2628 1.7-.7893 1.36.0729.1093.1882-.0183 2.8535-.607 1.5421-.2794 1.8396-.3157.8318.3886.091.3946-.3278.8075-1.967.4857-2.3072.4614-3.4364.8136-.0425.0304.0486.0607 1.5482.1457.6618.0364h1.621l3.0175.2247.7892.522.4736.6376-.079.4857-1.2142.6193-1.6393-.3886-3.825-.9107-1.3113-.3279h-.1822v.1093l1.0929 1.0686 2.0035 1.8092 2.5075 2.3314.1275.5768-.3218.4554-.34-.0486-2.2039-1.6575-.85-.7468-1.9246-1.621h-.1275v.17l.4432.6496 2.3436 3.5214.1214 1.0807-.17.3521-.6071.2125-.6679-.1214-1.3721-1.9246L14.38 17.959l-1.1414-1.9428-.1397.079-.674 7.2552-.3156.3703-.7286.2793-.6071-.4614-.3218-.7468.3218-1.4753.3886-1.9246.3157-1.53.2853-1.9004.17-.6314-.0121-.0425-.1397.0182-1.4328 1.9672-2.1796 2.9446-1.7243 1.8456-.4128.164-.7164-.3704.0667-.6618.4008-.5889 2.386-3.0357 1.4389-1.882.929-1.0868-.0062-.1579h-.0546l-6.3385 4.1164-1.1293.1457-.4857-.4554.0608-.7467.2307-.2429 1.9064-1.3114Z";

const OPENAI_PATH =
  "M22.2819 9.8211a5.9847 5.9847 0 0 0-.5157-4.9108 6.0462 6.0462 0 0 0-6.5098-2.9A6.0651 6.0651 0 0 0 4.9807 4.1818a5.9847 5.9847 0 0 0-3.9977 2.9 6.0462 6.0462 0 0 0 .7427 7.0966 5.98 5.98 0 0 0 .511 4.9107 6.051 6.051 0 0 0 6.5146 2.9001A5.9847 5.9847 0 0 0 13.2599 24a6.0557 6.0557 0 0 0 5.7718-4.2058 5.9894 5.9894 0 0 0 3.9977-2.9001 6.0557 6.0557 0 0 0-.7475-7.0729zm-9.022 12.6081a4.4755 4.4755 0 0 1-2.8764-1.0408l.1419-.0804 4.7783-2.7582a.7948.7948 0 0 0 .3927-.6813v-6.7369l2.02 1.1686a.071.071 0 0 1 .038.052v5.5826a4.504 4.504 0 0 1-4.4945 4.4944zm-9.6607-4.1254a4.4708 4.4708 0 0 1-.5346-3.0137l.142.0852 4.783 2.7582a.7712.7712 0 0 0 .7806 0l5.8428-3.3685v2.3324a.0804.0804 0 0 1-.0332.0615L9.74 19.9502a4.4992 4.4992 0 0 1-6.1408-1.6464zM2.3408 7.8956a4.485 4.485 0 0 1 2.3655-1.9728V11.6a.7664.7664 0 0 0 .3879.6765l5.8144 3.3543-2.0201 1.1685a.0757.0757 0 0 1-.071 0l-4.8303-2.7865A4.504 4.504 0 0 1 2.3408 7.872zm16.5963 3.8558L13.1038 8.364 15.1192 7.2a.0757.0757 0 0 1 .071 0l4.8303 2.7913a4.4944 4.4944 0 0 1-.6765 8.1042v-5.6772a.79.79 0 0 0-.407-.667zm2.0107-3.0231l-.142-.0852-4.7735-2.7818a.7759.7759 0 0 0-.7854 0L9.409 9.2297V6.8974a.0662.0662 0 0 1 .0284-.0615l4.8303-2.7866a4.4992 4.4992 0 0 1 6.6802 4.66zM8.3065 12.863l-2.02-1.1638a.0804.0804 0 0 1-.038-.0567V6.0742a4.4992 4.4992 0 0 1 7.3757-3.4537l-.142.0805L8.704 5.459a.7948.7948 0 0 0-.3927.6813zm1.0976-2.3654l2.602-1.4998 2.6069 1.4998v2.9994l-2.5974 1.4997-2.6067-1.4997Z";

function brandSvg(path: string, { size = 14, className }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="currentColor"
      className={className}
      aria-hidden="true"
    >
      <path d={path} />
    </svg>
  );
}

/** Official Claude (Anthropic) mark. */
export const ClaudeMark = (p: IconProps) => brandSvg(CLAUDE_PATH, p);

/** Official OpenAI mark (Codex runs on OpenAI models). */
export const CodexMark = (p: IconProps) => brandSvg(OPENAI_PATH, p);

/** Official Kimi Code robot mark. */
export const KimiMark = (p: IconProps) =>
  svg(
    <>
      <rect x="3" y="4.5" width="18" height="13" rx="2.2" />
      <rect x="9.6" y="8" width="1.4" height="2.6" rx=".45" fill="currentColor" stroke="none" />
      <rect x="15.6" y="8" width="1.4" height="2.6" rx=".45" fill="currentColor" stroke="none" />
    </>,
    p,
  );

/** Hermes Agent (remote gateways): a winged caduceus-style staff. */
const HERMES_PATH =
  "M12 1.5l1.9 2.9c1.6-1.1 3.7-1.5 5.9-.9-.9 2-2.6 3.4-4.7 3.9l-2.1-.5v2.3l3.4-1c1.7.4 3.2 1.5 4.1 3.2-1.7.5-3.5.3-5-.5l-2.5-.8v2.4l2.6.9c1.3.4 2.4 1.3 3.1 2.6-1.4.4-2.9.2-4.1-.5l-1.6-.8V22.5h-2v-7.8l-1.6.8c-1.2.7-2.7.9-4.1.5.7-1.3 1.8-2.2 3.1-2.6l2.6-.9V10l-2.5.8c-1.5.8-3.3 1-5 .5.9-1.7 2.4-2.8 4.1-3.2l3.4 1V6.9l-2.1.5C6.8 6.9 5.1 5.5 4.2 3.5c2.2-.6 4.3-.2 5.9.9L12 1.5z";
export const HermesMark = (p: IconProps) => brandSvg(HERMES_PATH, p);

/** Claudex: the Claude harness on someone else's model — a bridge, drawn as
 *  two opposed arrows. Deliberately NOT the Claude mark: at sidebar size these
 *  threads must not read as ordinary Anthropic-backed Claude sessions. */
const CLAUDEX_PATH =
  "M3 6.2h13V3.4L21.6 7 16 10.6V7.8H3V6.2z M21 16.2H8v-2.8L2.4 17 8 20.6v-2.8h13v-1.6z";
export const ClaudexMark = (p: IconProps) => brandSvg(CLAUDEX_PATH, p);

/** Brand glyph for an agent id, pre-tinted via provider mark classes. */
export function AgentMark({
  agent,
  size = 14,
  className,
}: {
  agent: string;
  size?: number;
  className?: string;
}) {
  const cls = (base: string) => (className ? `${base} ${className}` : base);
  if (agent === "claude") return <ClaudeMark size={size} className={cls("mark mark-claude")} />;
  if (agent === "kimi") return <KimiMark size={size} className={cls("mark mark-kimi")} />;
  if (agent === "hermes") return <HermesMark size={size} className={cls("mark mark-hermes")} />;
  if (agent === "claudex") return <ClaudexMark size={size} className={cls("mark mark-claudex")} />;
  return <CodexMark size={size} className={cls("mark mark-codex")} />;
}

/** Map a tool name to a glyph. */
export function ToolGlyph({ name, size = 14 }: { name: string; size?: number }) {
  const n = name.toLowerCase();
  if (/(bash|shell|exec|command|terminal|run)/.test(n)) return <TerminalIcon size={size} />;
  if (/(edit|write|patch|apply|create)/.test(n)) return <PencilIcon size={size} />;
  if (/(read|cat|open|view|file|notebook)/.test(n)) return <FileIcon size={size} />;
  if (/(grep|search|glob|find|ls|list)/.test(n)) return <SearchIcon size={size} />;
  if (/(web|fetch|http|browse|url)/.test(n)) return <GlobeIcon size={size} />;
  if (/(todo|task|plan)/.test(n)) return <CheckIcon size={size} />;
  return <WrenchIcon size={size} />;
}

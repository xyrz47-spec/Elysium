// 가는 선 아이콘(currentColor로 그린다).
import type { ReactNode } from "react";

const Svg = ({ children, size = 18 }: { children: ReactNode; size?: number }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.7} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
    {children}
  </svg>
);

export const SendIcon = () => (
  <Svg>
    <path d="M12 19V5M6 11l6-6 6 6" />
  </Svg>
);
export const StopIcon = () => (
  <Svg size={16}>
    <rect x="7" y="7" width="10" height="10" rx="2" fill="currentColor" />
  </Svg>
);
export const PlusIcon = () => (
  <Svg>
    <path d="M12 5v14M5 12h14" />
  </Svg>
);
export const GearIcon = () => (
  <Svg>
    <circle cx="12" cy="12" r="3" />
    <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z" />
  </Svg>
);
export const SparkIcon = ({ size = 18 }: { size?: number }) => (
  <Svg size={size}>
    <path d="M12 3l1.8 5.2L19 10l-5.2 1.8L12 17l-1.8-5.2L5 10l5.2-1.8z" />
    <path d="M19 15l.7 2 2 .7-2 .7-.7 2-.7-2-2-.7 2-.7z" />
  </Svg>
);
export const PuzzleIcon = () => (
  <Svg>
    <path d="M10 4a2 2 0 1 1 4 0v1h3a1 1 0 0 1 1 1v3h1a2 2 0 1 1 0 4h-1v3a1 1 0 0 1-1 1h-3v-1a2 2 0 1 0-4 0v1H7a1 1 0 0 1-1-1v-3H5a2 2 0 1 1 0-4h1V6a1 1 0 0 1 1-1h3z" />
  </Svg>
);
export const TrashIcon = () => (
  <Svg size={15}>
    <path d="M4 7h16M10 11v6M14 11v6M6 7l1 12a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2l1-12M9 7V4h6v3" />
  </Svg>
);
export const CopyIcon = () => (
  <Svg size={15}>
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M5 15V6a2 2 0 0 1 2-2h8" />
  </Svg>
);
export const ChevronIcon = ({ open }: { open?: boolean }) => (
  <Svg size={14}>
    <path d={open ? "M6 15l6-6 6 6" : "M6 9l6 6 6-6"} />
  </Svg>
);
export const FolderIcon = () => (
  <Svg size={15}>
    <path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
  </Svg>
);
export const RefreshIcon = () => (
  <Svg size={15}>
    <path d="M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7" />
  </Svg>
);
export const CloseIcon = () => (
  <Svg size={16}>
    <path d="M6 6l12 12M18 6L6 18" />
  </Svg>
);
export const ChatIcon = () => (
  <Svg size={15}>
    <path d="M4 5h16v11H8l-4 4z" />
  </Svg>
);
export const WrenchIcon = () => (
  <Svg size={15}>
    <path d="M14.7 6.3a4 4 0 0 0-5.4 5.4L3 18l3 3 6.3-6.3a4 4 0 0 0 5.4-5.4l-2.5 2.5-2.5-.5-.5-2.5z" />
  </Svg>
);
export const SunIcon = () => (
  <Svg size={16}>
    <circle cx="12" cy="12" r="4" />
    <path d="M12 2v2M12 20v2M2 12h2M20 12h2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" />
  </Svg>
);
export const MoonIcon = () => (
  <Svg size={16}>
    <path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z" />
  </Svg>
);
export const AutoIcon = () => (
  <Svg size={16}>
    <circle cx="12" cy="12" r="8" />
    <path d="M12 4v16a8 8 0 0 0 0-16z" fill="currentColor" />
  </Svg>
);
export const CheckIcon = () => (
  <Svg size={14}>
    <path d="M5 12l5 5 9-10" />
  </Svg>
);
export const PanelIcon = () => (
  <Svg>
    <rect x="3.5" y="4.5" width="17" height="15" rx="3" />
    <path d="M9.5 4.5v15" />
  </Svg>
);
export const ChartIcon = () => (
  <Svg>
    <path d="M4 20h16M7 16v-4M12 16V7M17 16v-6" />
  </Svg>
);
export const InfoIcon = () => (
  <Svg>
    <circle cx="12" cy="12" r="8.5" />
    <path d="M12 11v5.5M12 7.6v.1" />
  </Svg>
);
export const ListIcon = () => (
  <Svg>
    <path d="M9 6.5h11M9 12h11M9 17.5h11M4.5 6.5h.01M4.5 12h.01M4.5 17.5h.01" />
  </Svg>
);
export const SearchIcon = () => (
  <Svg>
    <circle cx="11" cy="11" r="6.5" />
    <path d="m20 20-4.2-4.2" />
  </Svg>
);
export const BookmarkIcon = () => (
  <Svg>
    <path d="M7 4.5h10v15l-5-3.5-5 3.5z" />
  </Svg>
);
export const DownloadIcon = () => (
  <Svg>
    <path d="M12 4.5v10M7.5 10.5 12 15l4.5-4.5M5 19.5h14" />
  </Svg>
);
export const KeyboardIcon = () => (
  <Svg>
    <rect x="3" y="6.5" width="18" height="11" rx="2.5" />
    <path d="M7 10.5h.01M10.5 10.5h.01M14 10.5h.01M17.5 10.5h.01M8 14h8" />
  </Svg>
);
export const ArrowUpIcon = () => (
  <Svg size={16}>
    <path d="M12 19V5M6 11l6-6 6 6" />
  </Svg>
);
export const ArrowDownIcon = () => (
  <Svg size={16}>
    <path d="M12 5v14M6 13l6 6 6-6" />
  </Svg>
);

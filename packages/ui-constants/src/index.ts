// Color palette used across UI components (OKLCH — perceptually uniform)
export const COLORS = {
	primary: "oklch(0.70 0.175 44)", // ccflare orange
	success: "oklch(0.70 0.150 168)", // green
	warning: "oklch(0.80 0.155 90)", // yellow — H90 avoids confusion with orange primary (H44)
	error: "oklch(0.60 0.215 22)", // red
	blue: "oklch(0.62 0.190 255)",
	purple: "oklch(0.60 0.185 300)",
	pink: "oklch(0.64 0.205 350)",
	indigo: "oklch(0.65 0.190 275)", // raised from L0.57 — readable on dark backgrounds
	cyan: "oklch(0.74 0.138 198)",
} as const;

// Chart color sequence for multi-series charts
export const CHART_COLORS = [
	COLORS.primary,
	COLORS.blue,
	COLORS.purple,
	COLORS.pink,
	COLORS.success,
] as const;

// Time range options for analytics
export type TimeRange = "1h" | "6h" | "24h" | "7d" | "30d";

export const TIME_RANGES: Record<TimeRange, string> = {
	"1h": "Last Hour",
	"6h": "Last 6 Hours",
	"24h": "Last 24 Hours",
	"7d": "Last 7 Days",
	"30d": "Last 30 Days",
} as const;

// Chart dimensions
export const CHART_HEIGHTS = {
	small: 250,
	medium: 300,
	large: 400,
} as const;

// Common chart tooltip styles
export const CHART_TOOLTIP_STYLE = {
	default: {
		backgroundColor: "var(--background)",
		border: "1px solid var(--border)",
		borderRadius: "var(--radius)",
	},
	success: {
		backgroundColor: COLORS.success,
		border: `1px solid ${COLORS.success}`,
		borderRadius: "var(--radius)",
		color: "oklch(1.00 0 0)",
	},
	dark: {
		backgroundColor: "oklch(0 0 0 / 0.8)",
		border: "1px solid oklch(1.00 0 0 / 0.2)",
		borderRadius: "8px",
		backdropFilter: "blur(8px)",
	},
} as const;

// Chart common properties
export const CHART_PROPS = {
	strokeDasharray: "3 3",
	gridClassName: "stroke-muted",
} as const;

// API and data refresh intervals (in milliseconds)
export const REFRESH_INTERVALS = {
	default: 30000, // 30 seconds
	fast: 10000, // 10 seconds
	slow: 60000, // 1 minute
} as const;

// API timeout
export const API_TIMEOUT = 30000; // 30 seconds

// React Query configuration
export const QUERY_CONFIG = {
	staleTime: 10000, // Consider data stale after 10 seconds
} as const;

// API default limits
export const API_LIMITS = {
	requestsDetail: 50,
	requestsSummary: 50,
} as const;

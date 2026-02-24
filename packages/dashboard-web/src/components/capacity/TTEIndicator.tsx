import { cn } from "../../lib/utils";
import type { TteMinutes } from "../../types/capacity";

interface TTEIndicatorProps {
	tteMinutes: TteMinutes;
}

function formatTte(minutes: number | null): string {
	if (minutes === null || minutes > 1440) return "—";
	if (minutes < 1) return "<1m";
	if (minutes < 60) return `${Math.round(minutes)}m`;
	return `${(minutes / 60).toFixed(1)}h`;
}

function tteColor(minutes: number | null): string {
	if (minutes === null || minutes > 1440) return "text-muted-foreground";
	if (minutes < 5) return "text-destructive";
	if (minutes < 15) return "text-yellow-600 dark:text-yellow-400";
	return "text-green-600 dark:text-green-400";
}

function tteBg(minutes: number | null): string {
	if (minutes === null || minutes > 1440) return "bg-muted/50";
	if (minutes < 5) return "bg-destructive/10";
	if (minutes < 15) return "bg-yellow-500/10";
	return "bg-green-500/10";
}

export function TTEIndicator({ tteMinutes }: TTEIndicatorProps) {
	const { current, withWindowRecovery } = tteMinutes;
	const showRecovery =
		withWindowRecovery !== null &&
		current !== null &&
		withWindowRecovery - current > 2;

	const isCritical = current !== null && current < 5;

	const tooltip = [
		current !== null
			? `TTE at current rate: ${formatTte(current)}`
			: "TTE: unknown",
		withWindowRecovery !== null
			? `With 15min window recovery: ${formatTte(withWindowRecovery)}`
			: null,
	]
		.filter(Boolean)
		.join(" | ");

	return (
		<span
			className={cn(
				"inline-flex items-center rounded overflow-hidden font-mono text-xs select-none",
				isCritical && "animate-pulse",
			)}
			title={tooltip}
		>
			<span
				className={cn(
					"px-1.5 py-0.5 font-medium",
					tteBg(current),
					tteColor(current),
				)}
			>
				{formatTte(current)}
			</span>
			{showRecovery && (
				<span className="px-1.5 py-0.5 bg-muted/30 text-muted-foreground">
					+{formatTte(withWindowRecovery! - current!)}
				</span>
			)}
		</span>
	);
}

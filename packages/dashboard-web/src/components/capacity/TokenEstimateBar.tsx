import type { ConfidenceLevel, RemainingTokens } from "../../types/capacity";

interface TokenEstimateBarProps {
	remaining: RemainingTokens;
	capacity: number; // estimated total capacity (expected + used)
	confidence: ConfidenceLevel;
}

function formatK(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
	return String(Math.round(n));
}

export function TokenEstimateBar({
	remaining,
	capacity,
	confidence,
}: TokenEstimateBarProps) {
	if (capacity <= 0) return null;

	const pessimisticPct = Math.min(
		100,
		(remaining.pessimistic / capacity) * 100,
	);
	const expectedPct = Math.min(100, (remaining.expected / capacity) * 100);
	const optimisticPct = Math.min(100, (remaining.optimistic / capacity) * 100);

	const isCold = confidence === "cold";

	const tooltip = isCold
		? `Estimate based on prior only (no observations)`
		: `Remaining: ${formatK(remaining.pessimistic)}–${formatK(remaining.expected)}–${formatK(remaining.optimistic)} tokens (pessimistic/expected/optimistic)`;

	return (
		<div className="space-y-1" title={tooltip}>
			<div className="relative h-2.5 rounded-full bg-muted overflow-visible">
				{/* Uncertainty band: pessimistic to optimistic */}
				<div
					className="absolute top-0 h-full rounded-full bg-primary/10"
					style={{ left: "0%", width: `${optimisticPct}%` }}
				/>
				{/* Expected tick */}
				<div
					className={`absolute top-0 h-full rounded-full ${isCold ? "opacity-30" : ""}`}
					style={{
						left: "0%",
						width: `${pessimisticPct}%`,
						background: isCold
							? "repeating-linear-gradient(45deg, hsl(var(--muted-foreground)/0.3) 0px, hsl(var(--muted-foreground)/0.3) 2px, transparent 2px, transparent 4px)"
							: "hsl(var(--primary)/0.25)",
					}}
				/>
				{/* Expected marker line */}
				{!isCold && (
					<div
						className="absolute top-0 w-0.5 h-full bg-primary rounded-full"
						style={{ left: `${expectedPct}%` }}
					/>
				)}
			</div>
			<div className="flex justify-between text-xs text-muted-foreground font-mono">
				<span>{formatK(remaining.pessimistic)}</span>
				<span className="text-foreground font-medium">
					{formatK(remaining.expected)}
				</span>
				<span>{formatK(remaining.optimistic)}</span>
			</div>
		</div>
	);
}

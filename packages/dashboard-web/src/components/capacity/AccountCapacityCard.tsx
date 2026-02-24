import { AlertTriangle } from "lucide-react";
import { cn } from "../../lib/utils";
import type { AccountCapacity } from "../../types/capacity";
import { Badge } from "../ui/badge";
import { Card, CardContent } from "../ui/card";
import { ConfidenceBadge } from "./ConfidenceBadge";
import { TokenEstimateBar } from "./TokenEstimateBar";
import { TTEIndicator } from "./TTEIndicator";

interface AccountCapacityCardProps {
	account: AccountCapacity;
}

function utilizationColor(pct: number): string {
	if (pct >= 85) return "text-destructive";
	if (pct >= 60) return "text-yellow-600 dark:text-yellow-400";
	return "text-green-600 dark:text-green-400";
}

function utilizationBorder(shouldStop: boolean, pct: number): string {
	if (shouldStop) return "border-destructive";
	if (pct >= 85) return "border-yellow-500/50";
	return "border-border";
}

export function AccountCapacityCard({ account }: AccountCapacityCardProps) {
	const {
		name,
		provider,
		utilization5hPct,
		remaining5h,
		tteMinutes,
		confidence,
		nEff,
		shouldStopAssign,
		sharedScore,
		suspectedShared,
		windowRecovery15minTokens,
	} = account;

	// Estimate total capacity from remaining + utilization
	const estimatedCapacity =
		utilization5hPct > 0 && utilization5hPct < 100
			? remaining5h.expected / ((100 - utilization5hPct) / 100)
			: remaining5h.expected;

	const utilizationPct = Math.round(utilization5hPct);

	return (
		<Card
			className={cn(
				"transition-colors",
				utilizationBorder(shouldStopAssign, utilization5hPct),
				shouldStopAssign && "bg-destructive/5",
			)}
		>
			<CardContent className="p-4 space-y-3">
				{/* Header */}
				<div className="flex items-start justify-between gap-2">
					<div className="min-w-0">
						<p className="font-medium truncate" title={name}>
							{name}
						</p>
						<p className="text-xs text-muted-foreground">{provider}</p>
					</div>
					<div className="flex flex-wrap items-center gap-1 shrink-0">
						<ConfidenceBadge confidence={confidence} nEff={nEff} />
						{suspectedShared && (
							<Badge
								variant="warning"
								className="text-xs"
								title={`Shared score: ${sharedScore.toFixed(2)} — external usage detected`}
							>
								Shared?
							</Badge>
						)}
						{shouldStopAssign && (
							<Badge variant="destructive" className="text-xs gap-1">
								<AlertTriangle className="h-3 w-3" />
								Stop
							</Badge>
						)}
					</div>
				</div>

				{/* Utilization + TTE row */}
				<div className="flex items-center justify-between">
					<div className="flex items-center gap-2">
						<span
							className={cn(
								"text-2xl font-bold font-mono tabular-nums",
								utilizationColor(utilization5hPct),
							)}
							title="5h window utilization"
						>
							{utilizationPct}%
						</span>
						<span className="text-xs text-muted-foreground">used (5h)</span>
					</div>
					<TTEIndicator tteMinutes={tteMinutes} />
				</div>

				{/* Remaining token range bar */}
				<TokenEstimateBar
					remaining={remaining5h}
					capacity={estimatedCapacity}
					confidence={confidence}
				/>

				{/* Recovery info */}
				{windowRecovery15minTokens > 0 && (
					<p
						className="text-xs text-muted-foreground"
						title="Tokens that will expire from the 5h window in the next 15 minutes, freeing up capacity"
					>
						+
						{windowRecovery15minTokens >= 1000
							? `${Math.round(windowRecovery15minTokens / 1000)}K`
							: String(Math.round(windowRecovery15minTokens))}{" "}
						tokens freeing in 15min
					</p>
				)}
			</CardContent>
		</Card>
	);
}

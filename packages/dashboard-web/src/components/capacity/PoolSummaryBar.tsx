import { cn } from "../../lib/utils";
import type { AccountCapacity, PoolTotal } from "../../types/capacity";
import { Card, CardContent } from "../ui/card";

interface PoolSummaryBarProps {
	poolTotal: PoolTotal;
	accounts: AccountCapacity[];
}

function formatTokens(n: number): string {
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
	return String(Math.round(n));
}

function formatTte(minutes: number | null): string {
	if (minutes === null || minutes > 1440) return "—";
	if (minutes < 1) return "<1m";
	if (minutes < 60) return `${Math.round(minutes)}m`;
	return `${(minutes / 60).toFixed(1)}h`;
}

interface MetricCardProps {
	label: string;
	value: string;
	sub?: string;
	valueClass?: string;
}

function MetricCard({ label, value, sub, valueClass }: MetricCardProps) {
	return (
		<Card>
			<CardContent className="p-4">
				<p className="text-xs text-muted-foreground uppercase tracking-wide mb-1">
					{label}
				</p>
				<p
					className={cn(
						"text-2xl font-bold font-mono tabular-nums",
						valueClass ?? "text-foreground",
					)}
				>
					{value}
				</p>
				{sub && <p className="text-xs text-muted-foreground mt-0.5">{sub}</p>}
			</CardContent>
		</Card>
	);
}

export function PoolSummaryBar({ poolTotal, accounts }: PoolSummaryBarProps) {
	const accountsAtRisk = accounts.filter((a) => a.shouldStopAssign).length;
	const totalRate = accounts.reduce(
		(sum, a) => sum + a.kfExternalRateTokensPerSec,
		0,
	);

	const tteColor =
		poolTotal.soonestTteMinutes === null || poolTotal.soonestTteMinutes > 60
			? "text-green-600 dark:text-green-400"
			: poolTotal.soonestTteMinutes < 10
				? "text-destructive"
				: "text-yellow-600 dark:text-yellow-400";

	return (
		<div className="grid grid-cols-2 md:grid-cols-4 gap-4">
			<MetricCard
				label="Pool Remaining (p50)"
				value={formatTokens(poolTotal.remainingExpected)}
				sub={`${formatTokens(poolTotal.remainingPessimistic)} pessimistic`}
				valueClass="text-green-600 dark:text-green-400"
			/>
			<MetricCard
				label="Soonest TTE"
				value={formatTte(poolTotal.soonestTteMinutes)}
				sub={
					poolTotal.soonestTteWithRecoveryMinutes !== null
						? `+${formatTte(poolTotal.soonestTteWithRecoveryMinutes)} with recovery`
						: undefined
				}
				valueClass={tteColor}
			/>
			<MetricCard
				label="Accounts at Risk"
				value={String(accountsAtRisk)}
				sub={`of ${accounts.length} total`}
				valueClass={accountsAtRisk > 0 ? "text-destructive" : "text-foreground"}
			/>
			<MetricCard
				label="External Rate"
				value={totalRate > 0.01 ? `${totalRate.toFixed(1)} tok/s` : "—"}
				sub="KF-estimated external usage"
			/>
		</div>
	);
}

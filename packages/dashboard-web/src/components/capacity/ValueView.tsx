import { AlertCircle, TrendingDown, TrendingUp } from "lucide-react";
import {
	Bar,
	BarChart,
	CartesianGrid,
	Cell,
	ReferenceLine,
	ResponsiveContainer,
	Tooltip,
	XAxis,
	YAxis,
} from "recharts";
import { useValue } from "../../hooks/capacity-queries";
import type { AccountValue } from "../../types/capacity";
import { Badge } from "../ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "../ui/card";
import { Progress } from "../ui/progress";
import { Skeleton } from "../ui/skeleton";

const PAYG_BLENDED_CPM = 6.6; // $/M tokens blended

function formatCurrency(n: number): string {
	if (Math.abs(n) >= 1000) return `$${(n / 1000).toFixed(1)}K`;
	if (Math.abs(n) >= 1) return `$${n.toFixed(2)}`;
	return `$${n.toFixed(4)}`;
}

function formatTokens(n: number): string {
	if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
	if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
	if (n >= 1_000) return `${Math.round(n / 1_000)}K`;
	return String(Math.round(n));
}

function RecommendationBadge({ rec }: { rec: string }) {
	const config: Record<
		string,
		{
			label: string;
			variant: "success" | "warning" | "destructive" | "secondary" | "outline";
		}
	> = {
		GOOD_VALUE: { label: "Good Value", variant: "success" },
		UNDERUTILIZED: { label: "Underutilized", variant: "warning" },
		NEAR_EXHAUSTION_5H: { label: "Near Exhaustion", variant: "destructive" },
		SUSPECTED_SHARED: { label: "Suspected Shared", variant: "warning" },
	};
	const c = config[rec];
	if (!c)
		return (
			<Badge variant="outline" className="text-xs">
				{rec.replace(/_/g, " ").toLowerCase()}
			</Badge>
		);
	return (
		<Badge variant={c.variant} className="text-xs">
			{c.label}
		</Badge>
	);
}

interface CpmTooltipProps {
	active?: boolean;
	payload?: Array<{
		payload: AccountValue & { cpmActual: number; cpmPayg: number };
	}>;
}

function CpmTooltip({ active, payload }: CpmTooltipProps) {
	if (!active || !payload?.length) return null;
	const acc = payload[0].payload;
	return (
		<div className="bg-popover border border-border rounded-lg p-3 text-xs shadow-lg space-y-1">
			<p className="font-semibold">{acc.name}</p>
			{acc.cpmActualUsd != null && (
				<p>Actual: {formatCurrency(acc.cpmActualUsd)}/M tokens</p>
			)}
			{acc.cpmPaygEquivUsd != null && (
				<p>PAYG equiv: {formatCurrency(acc.cpmPaygEquivUsd)}/M tokens</p>
			)}
			{acc.cpmTheoreticalUsd != null && (
				<p>
					Theoretical best: {formatCurrency(acc.cpmTheoreticalUsd)}/M tokens
				</p>
			)}
			{acc.realizedPct != null && (
				<p>Realized: {acc.realizedPct.toFixed(1)}%</p>
			)}
			{acc.savingsUsd30d != null && (
				<p>30d savings: {formatCurrency(acc.savingsUsd30d)}</p>
			)}
		</div>
	);
}

function SavingsCard({
	poolSummary,
}: {
	poolSummary: {
		totalSavingsUsd30d: number;
		totalPaygCostUsd30d: number;
		poolRealizedPct: number | null;
	};
}) {
	const { totalSavingsUsd30d, totalPaygCostUsd30d, poolRealizedPct } =
		poolSummary;
	const savingsRate =
		totalPaygCostUsd30d > 0
			? (totalSavingsUsd30d / totalPaygCostUsd30d) * 100
			: 0;
	const isPositive = totalSavingsUsd30d >= 0;

	return (
		<Card>
			<CardContent className="p-6">
				<p className="text-xs text-muted-foreground uppercase tracking-wide mb-2">
					Pool Savings (30d vs PAYG)
				</p>
				<div className="flex items-baseline gap-3 mb-3">
					<span
						className={`text-4xl font-bold font-mono tabular-nums ${isPositive ? "text-green-600 dark:text-green-400" : "text-destructive"}`}
					>
						{isPositive ? "+" : ""}
						{formatCurrency(totalSavingsUsd30d)}
					</span>
					{isPositive ? (
						<TrendingUp className="h-5 w-5 text-green-600 dark:text-green-400" />
					) : (
						<TrendingDown className="h-5 w-5 text-destructive" />
					)}
				</div>
				{savingsRate > 0 && (
					<>
						<Progress
							value={Math.min(100, savingsRate)}
							className="h-1.5 mb-1"
						/>
						<p className="text-xs text-muted-foreground">
							{savingsRate.toFixed(1)}% savings rate vs PAYG
						</p>
					</>
				)}
				{poolRealizedPct != null && (
					<p className="text-xs text-muted-foreground mt-1">
						{poolRealizedPct.toFixed(1)}% of potential subscription value
						realized
					</p>
				)}
				<p className="text-xs text-muted-foreground mt-1">
					{formatTokens(
						(poolSummary.totalPaygCostUsd30d / PAYG_BLENDED_CPM) * 1_000_000,
					)}{" "}
					tokens delivered across all accounts
				</p>
			</CardContent>
		</Card>
	);
}

export function ValueView() {
	const { data, isLoading, isError, error } = useValue();

	if (isLoading) {
		return (
			<div className="space-y-4">
				<Card>
					<CardContent className="p-6">
						<Skeleton className="h-24 w-full" />
					</CardContent>
				</Card>
				<Card>
					<CardContent className="p-4">
						<Skeleton className="h-48 w-full" />
					</CardContent>
				</Card>
			</div>
		);
	}

	if (isError) {
		return (
			<Card className="border-destructive">
				<CardContent className="pt-6">
					<div className="flex items-center gap-2">
						<AlertCircle className="h-4 w-4 text-destructive" />
						<p className="text-destructive">
							Failed to load value data.{" "}
							{error instanceof Error ? error.message : "Unknown error"}
						</p>
					</div>
				</CardContent>
			</Card>
		);
	}

	if (!data) return null;

	// Prepare CPM chart data — only accounts with known cost
	const cpmData = data.accounts
		.filter((a) => a.cpmActualUsd != null)
		.sort((a, b) => (a.cpmActualUsd ?? 0) - (b.cpmActualUsd ?? 0))
		.map((a) => ({
			...a,
			cpmActual: a.cpmActualUsd ?? 0,
			cpmPayg: a.cpmPaygEquivUsd ?? 0,
		}));

	const accountsNoCost = data.accounts.filter(
		(a) => a.costSource === "unknown" || a.cpmActualUsd == null,
	);

	return (
		<div className="space-y-6">
			<SavingsCard poolSummary={data.poolSummary} />

			{/* CPM comparison chart */}
			{cpmData.length > 0 && (
				<Card>
					<CardHeader>
						<CardTitle className="text-base">
							Cost per Million Tokens (30d)
						</CardTitle>
						<p className="text-xs text-muted-foreground">
							Actual cost vs PAYG equivalent. Dotted line = blended PAYG rate ($
							{PAYG_BLENDED_CPM}/M). Lower is better.
						</p>
					</CardHeader>
					<CardContent>
						<ResponsiveContainer
							width="100%"
							height={Math.max(160, cpmData.length * 52)}
						>
							<BarChart
								layout="vertical"
								data={cpmData}
								margin={{ left: 8, right: 24, top: 4, bottom: 4 }}
							>
								<CartesianGrid strokeDasharray="3 3" horizontal={false} />
								<XAxis
									type="number"
									tickFormatter={(v) => `$${v.toFixed(1)}`}
									tick={{ fontSize: 11 }}
									domain={[0, "auto"]}
								/>
								<YAxis
									type="category"
									dataKey="name"
									width={110}
									tick={{ fontSize: 12 }}
								/>
								<Tooltip content={<CpmTooltip />} />
								<ReferenceLine
									x={PAYG_BLENDED_CPM}
									stroke="hsl(var(--muted-foreground))"
									strokeDasharray="4 4"
									label={{
										value: "PAYG",
										fontSize: 10,
										fill: "hsl(var(--muted-foreground))",
									}}
								/>
								<Bar
									dataKey="cpmPayg"
									name="PAYG equiv"
									fill="hsl(var(--muted))"
									fillOpacity={0.5}
								/>
								<Bar dataKey="cpmActual" name="Actual" radius={[0, 2, 2, 0]}>
									{cpmData.map((entry) => {
										const isGood =
											entry.cpmActual <
											0.8 * (entry.cpmPayg ?? PAYG_BLENDED_CPM);
										const isBad =
											entry.cpmActual > (entry.cpmPayg ?? PAYG_BLENDED_CPM);
										return (
											<Cell
												key={entry.id}
												fill={
													isBad
														? "hsl(var(--destructive))"
														: isGood
															? "#22c55e"
															: "#eab308"
												}
											/>
										);
									})}
								</Bar>
							</BarChart>
						</ResponsiveContainer>
					</CardContent>
				</Card>
			)}

			{/* Per-account value table */}
			<Card>
				<CardHeader>
					<CardTitle className="text-base">Account Value Breakdown</CardTitle>
				</CardHeader>
				<CardContent>
					<div className="overflow-x-auto">
						<table className="w-full text-sm">
							<thead>
								<tr className="border-b text-muted-foreground text-xs">
									<th className="text-left py-2 pr-4 font-medium">Account</th>
									<th className="text-right py-2 pr-4 font-medium">
										Tokens (30d)
									</th>
									<th className="text-right py-2 pr-4 font-medium">
										Actual CPM
									</th>
									<th className="text-right py-2 pr-4 font-medium">PAYG CPM</th>
									<th className="text-right py-2 pr-4 font-medium">Realized</th>
									<th className="text-right py-2 pr-4 font-medium">Savings</th>
									<th className="text-left py-2 font-medium">Status</th>
								</tr>
							</thead>
							<tbody>
								{data.accounts.map((acc) => (
									<tr
										key={acc.id}
										className="border-b last:border-0 hover:bg-muted/30 transition-colors"
									>
										<td className="py-2 pr-4 font-medium">
											<div>{acc.name}</div>
											{acc.subscriptionTier && (
												<div className="text-xs text-muted-foreground">
													{acc.subscriptionTier}
												</div>
											)}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{formatTokens(acc.rawTokens30d)}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.cpmActualUsd != null
												? formatCurrency(acc.cpmActualUsd)
												: "—"}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.cpmPaygEquivUsd != null
												? formatCurrency(acc.cpmPaygEquivUsd)
												: "—"}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.realizedPct != null
												? `${acc.realizedPct.toFixed(1)}%`
												: "—"}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.savingsUsd30d != null ? (
												<span
													className={
														acc.savingsUsd30d >= 0
															? "text-green-600 dark:text-green-400"
															: "text-destructive"
													}
												>
													{acc.savingsUsd30d >= 0 ? "+" : ""}
													{formatCurrency(acc.savingsUsd30d)}
												</span>
											) : (
												"—"
											)}
										</td>
										<td className="py-2">
											<div className="flex flex-wrap gap-1">
												{acc.costSource === "unknown" && (
													<Badge
														variant="outline"
														className="text-xs"
														title="Set subscription tier to enable cost analysis"
													>
														No cost data
													</Badge>
												)}
												{acc.recommendations.map((r) => (
													<RecommendationBadge key={r} rec={r} />
												))}
											</div>
										</td>
									</tr>
								))}
							</tbody>
						</table>
					</div>
					{accountsNoCost.length > 0 && (
						<p className="text-xs text-muted-foreground mt-3">
							{accountsNoCost.length} account
							{accountsNoCost.length > 1 ? "s" : ""} without cost data:{" "}
							{accountsNoCost.map((a) => a.name).join(", ")}. Set subscription
							tier to enable savings analysis.
						</p>
					)}
				</CardContent>
			</Card>
		</div>
	);
}

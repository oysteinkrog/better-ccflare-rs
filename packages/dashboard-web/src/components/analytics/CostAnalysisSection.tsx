import { formatCost } from "@better-ccflare/ui-common";
import { DollarSign, Percent, PiggyBank, TrendingDown } from "lucide-react";
import { useMemo } from "react";
import {
	Area,
	AreaChart,
	CartesianGrid,
	Legend,
	ResponsiveContainer,
	Tooltip,
	XAxis,
	YAxis,
} from "recharts";
import type { TimeRange } from "../../constants";
import { CHART_HEIGHTS, CHART_PROPS, COLORS } from "../../constants";
import { formatCompactCurrency } from "../../lib/chart-utils";
import { ChartContainer } from "../charts/ChartContainer";
import { getTooltipStyles } from "../charts/chart-utils";
import {
	Card,
	CardContent,
	CardDescription,
	CardHeader,
	CardTitle,
} from "../ui/card";

interface CostAnalysisSectionProps {
	totals: {
		totalCostUsd: number;
		totalMoneySavedUsd: number;
	};
	timeSeries: Array<{
		time: string;
		cost: number;
		moneySaved: number;
	}>;
	costByAccount: Array<{
		name: string;
		provider: string;
		requests: number;
		costUsd: number;
		moneySavedUsd: number;
	}>;
	loading?: boolean;
	timeRange: TimeRange;
}

const PROVIDER_LABELS: Record<string, string> = {
	anthropic: "Claude OAuth (free)",
	"claude-console-api": "Claude API",
	zai: "Zai / Z.AI",
	minimax: "MiniMax",
	"anthropic-compatible": "Anthropic-compatible",
	"openai-compatible": "OpenAI-compatible",
	nanogpt: "NanoGPT",
	"vertex-ai": "Vertex AI",
	unknown: "Unknown",
};

function KpiCard({
	icon: Icon,
	label,
	value,
	sub,
	accent,
}: {
	icon: React.ElementType;
	label: string;
	value: string;
	sub?: string;
	accent?: string;
}) {
	return (
		<Card>
			<CardContent className="pt-6">
				<div className="flex items-start justify-between">
					<div className="space-y-1">
						<p className="text-sm text-muted-foreground">{label}</p>
						<p
							className="text-2xl font-bold tabular-nums"
							style={accent ? { color: accent } : undefined}
						>
							{value}
						</p>
						{sub && <p className="text-xs text-muted-foreground">{sub}</p>}
					</div>
					<div
						className="rounded-md p-2"
						style={{ background: `${accent ?? COLORS.primary}22` }}
					>
						<Icon
							className="h-5 w-5"
							style={{ color: accent ?? COLORS.primary }}
						/>
					</div>
				</div>
			</CardContent>
		</Card>
	);
}

export function CostAnalysisSection({
	totals,
	timeSeries,
	costByAccount,
	loading = false,
	timeRange,
}: CostAnalysisSectionProps) {
	const actualCost = totals.totalCostUsd - totals.totalMoneySavedUsd;
	const savingsRate =
		totals.totalCostUsd > 0
			? (totals.totalMoneySavedUsd / totals.totalCostUsd) * 100
			: 0;

	const isLongRange = timeRange === "7d" || timeRange === "30d";

	const hasData = totals.totalCostUsd > 0 || costByAccount.length > 0;

	// Sort accounts: free first, then by cost desc
	const sortedAccounts = useMemo(
		() =>
			[...costByAccount].sort((a, b) => {
				if (a.provider === "anthropic" && b.provider !== "anthropic") return -1;
				if (b.provider === "anthropic" && a.provider !== "anthropic") return 1;
				return b.costUsd - a.costUsd;
			}),
		[costByAccount],
	);

	return (
		<div className="space-y-6">
			<div>
				<h3 className="text-lg font-semibold">Cost Analysis</h3>
				<p className="text-sm text-muted-foreground">
					API-equivalent value delivered and money saved via free accounts
				</p>
			</div>

			{/* KPI cards */}
			<div className="grid grid-cols-2 lg:grid-cols-4 gap-4">
				<KpiCard
					icon={DollarSign}
					label="API-Equivalent Value"
					value={formatCost(totals.totalCostUsd)}
					sub="at standard Anthropic rates"
					accent={COLORS.primary}
				/>
				<KpiCard
					icon={PiggyBank}
					label="Money Saved"
					value={formatCost(totals.totalMoneySavedUsd)}
					sub="via free OAuth accounts"
					accent={COLORS.success}
				/>
				<KpiCard
					icon={Percent}
					label="Savings Rate"
					value={`${savingsRate.toFixed(1)}%`}
					sub="of API value delivered free"
					accent={
						savingsRate >= 80
							? COLORS.success
							: savingsRate >= 40
								? COLORS.warning
								: COLORS.error
					}
				/>
				<KpiCard
					icon={TrendingDown}
					label="Actual Cost"
					value={formatCost(actualCost)}
					sub="paid to provider APIs"
					accent={COLORS.blue}
				/>
			</div>

			{/* Dual-area chart */}
			<Card>
				<CardHeader>
					<CardTitle className="text-base">
						Cost vs. Savings Over Time
					</CardTitle>
					<CardDescription>
						API-equivalent value (orange) and money saved via free accounts
						(green)
					</CardDescription>
				</CardHeader>
				<CardContent>
					<ChartContainer
						loading={loading}
						height={CHART_HEIGHTS.medium}
						isEmpty={!hasData || timeSeries.length === 0}
						emptyState={
							<div className="text-muted-foreground text-sm">
								No cost data for this time range
							</div>
						}
					>
						<ResponsiveContainer width="100%" height={CHART_HEIGHTS.medium}>
							<AreaChart
								data={timeSeries}
								margin={{
									top: 10,
									right: 20,
									left: 10,
									bottom: isLongRange ? 50 : 20,
								}}
							>
								<defs>
									<linearGradient id="gradCost" x1="0" y1="0" x2="0" y2="1">
										<stop
											offset="0%"
											stopColor={COLORS.primary}
											stopOpacity={0.6}
										/>
										<stop
											offset="100%"
											stopColor={COLORS.primary}
											stopOpacity={0.05}
										/>
									</linearGradient>
									<linearGradient id="gradSaved" x1="0" y1="0" x2="0" y2="1">
										<stop
											offset="0%"
											stopColor={COLORS.success}
											stopOpacity={0.7}
										/>
										<stop
											offset="100%"
											stopColor={COLORS.success}
											stopOpacity={0.05}
										/>
									</linearGradient>
								</defs>
								<CartesianGrid
									strokeDasharray={CHART_PROPS.strokeDasharray}
									className={CHART_PROPS.gridClassName}
								/>
								<XAxis
									dataKey="time"
									fontSize={11}
									angle={isLongRange ? -45 : 0}
									textAnchor={isLongRange ? "end" : "middle"}
									height={isLongRange ? 55 : 25}
								/>
								<YAxis
									fontSize={11}
									tickFormatter={formatCompactCurrency}
									width={60}
								/>
								<Tooltip
									contentStyle={getTooltipStyles("default")}
									formatter={(value: number, name: string) => [
										formatCost(value),
										name,
									]}
								/>
								<Legend verticalAlign="top" height={32} />
								<Area
									type="monotone"
									dataKey="cost"
									name="API-equivalent value"
									stroke={COLORS.primary}
									strokeWidth={2}
									fill="url(#gradCost)"
								/>
								<Area
									type="monotone"
									dataKey="moneySaved"
									name="Money saved"
									stroke={COLORS.success}
									strokeWidth={2}
									fill="url(#gradSaved)"
								/>
							</AreaChart>
						</ResponsiveContainer>
					</ChartContainer>
				</CardContent>
			</Card>

			{/* Per-account breakdown table */}
			<Card>
				<CardHeader>
					<CardTitle className="text-base">Cost by Account</CardTitle>
					<CardDescription>
						API-equivalent value and savings per account
					</CardDescription>
				</CardHeader>
				<CardContent>
					{sortedAccounts.length === 0 ? (
						<p className="text-sm text-muted-foreground py-4 text-center">
							No account data for this time range
						</p>
					) : (
						<div className="overflow-x-auto">
							<table className="w-full text-sm">
								<thead>
									<tr className="border-b border-border">
										<th className="text-left py-2 pr-4 font-medium text-muted-foreground">
											Account
										</th>
										<th className="text-left py-2 pr-4 font-medium text-muted-foreground">
											Provider
										</th>
										<th className="text-right py-2 pr-4 font-medium text-muted-foreground tabular-nums">
											Requests
										</th>
										<th className="text-right py-2 pr-4 font-medium text-muted-foreground tabular-nums">
											API Value
										</th>
										<th className="text-right py-2 pr-4 font-medium text-muted-foreground tabular-nums">
											Saved
										</th>
										<th className="text-right py-2 font-medium text-muted-foreground tabular-nums">
											Actual Cost
										</th>
									</tr>
								</thead>
								<tbody>
									{sortedAccounts.map((account) => {
										const isFree = account.provider === "anthropic";
										const actualAccountCost =
											account.costUsd - account.moneySavedUsd;
										return (
											<tr
												key={account.name}
												className="border-b border-border/50 hover:bg-muted/30"
											>
												<td className="py-2.5 pr-4 font-medium">
													{account.name}
												</td>
												<td className="py-2.5 pr-4">
													<span
														className="inline-flex items-center gap-1 text-xs px-2 py-0.5 rounded-full font-medium"
														style={{
															background: isFree
																? `${COLORS.success}22`
																: `${COLORS.blue}18`,
															color: isFree ? COLORS.success : COLORS.blue,
														}}
													>
														{isFree && "✓ "}
														{PROVIDER_LABELS[account.provider] ??
															account.provider}
													</span>
												</td>
												<td className="py-2.5 pr-4 text-right tabular-nums">
													{account.requests.toLocaleString()}
												</td>
												<td className="py-2.5 pr-4 text-right tabular-nums">
													{formatCost(account.costUsd)}
												</td>
												<td
													className="py-2.5 pr-4 text-right tabular-nums font-medium"
													style={{
														color:
															account.moneySavedUsd > 0
																? COLORS.success
																: undefined,
													}}
												>
													{account.moneySavedUsd > 0
														? formatCost(account.moneySavedUsd)
														: "—"}
												</td>
												<td className="py-2.5 text-right tabular-nums">
													{actualAccountCost > 0.00005
														? formatCost(actualAccountCost)
														: "$0.0000"}
												</td>
											</tr>
										);
									})}
								</tbody>
							</table>
						</div>
					)}
				</CardContent>
			</Card>
		</div>
	);
}

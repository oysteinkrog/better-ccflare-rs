import { AlertCircle } from "lucide-react";
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
import { useXFactor } from "../../hooks/capacity-queries";
import type { XFactorAccount } from "../../types/capacity";
import { Badge } from "../ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "../ui/card";
import { Skeleton } from "../ui/skeleton";
import { ConfidenceBadge } from "./ConfidenceBadge";

function confidenceOpacity(nEff: number): number {
	if (nEff >= 10) return 1.0;
	if (nEff >= 4) return 0.75;
	if (nEff >= 1) return 0.5;
	return 0.3;
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
		NEAR_EXHAUSTION_7D: { label: "Near 7d Limit", variant: "warning" },
		SUSPECTED_SHARED: { label: "Suspected Shared", variant: "warning" },
		HIGH_MONTHLY_COST: { label: "High Cost", variant: "secondary" },
	};
	const c = config[rec];
	if (!c) {
		return (
			<Badge variant="outline" className="text-xs">
				{rec.replace(/_/g, " ").toLowerCase()}
			</Badge>
		);
	}
	return (
		<Badge variant={c.variant} className="text-xs">
			{c.label}
		</Badge>
	);
}

interface CustomTooltipProps {
	active?: boolean;
	payload?: Array<{
		payload: XFactorAccount & { lo: number; hi: number; mid: number };
	}>;
}

function CustomTooltip({ active, payload }: CustomTooltipProps) {
	if (!active || !payload?.length) return null;
	const acc = payload[0].payload;
	return (
		<div className="bg-popover border border-border rounded-lg p-3 text-xs shadow-lg space-y-1">
			<p className="font-semibold">{acc.name}</p>
			<p>
				X-factor: {acc.xFactor.lo.toFixed(1)}×&nbsp;–&nbsp;
				{acc.xFactor.mid.toFixed(1)}×&nbsp;–&nbsp;{acc.xFactor.hi.toFixed(1)}×
				(lo/mid/hi)
			</p>
			<p>
				Capacity 5h: {Math.round(acc.c5hTokens.lo / 1000)}K –{" "}
				{Math.round(acc.c5hTokens.mid / 1000)}K –{" "}
				{Math.round(acc.c5hTokens.hi / 1000)}K tokens
			</p>
			<p>
				Confidence: {acc.confidence} (nEff={acc.nEff.toFixed(1)})
			</p>
			{acc.suspectedShared && (
				<p className="text-yellow-600 dark:text-yellow-400">
					⚠ Suspected shared account
				</p>
			)}
		</div>
	);
}

function XFactorChart({ accounts }: { accounts: XFactorAccount[] }) {
	const sorted = [...accounts].sort((a, b) => b.xFactor.mid - a.xFactor.mid);

	const chartData = sorted.map((acc) => ({
		...acc,
		lo: acc.xFactor.lo,
		mid: acc.xFactor.mid,
		hi: acc.xFactor.hi,
		range: [acc.xFactor.lo, acc.xFactor.hi] as [number, number],
	}));

	return (
		<ResponsiveContainer
			width="100%"
			height={Math.max(180, sorted.length * 48)}
		>
			<BarChart
				layout="vertical"
				data={chartData}
				margin={{ left: 8, right: 24, top: 4, bottom: 4 }}
			>
				<CartesianGrid strokeDasharray="3 3" horizontal={false} />
				<XAxis
					type="number"
					domain={[0, "auto"]}
					tickFormatter={(v) => `${v}×`}
					tick={{ fontSize: 11 }}
				/>
				<YAxis
					type="category"
					dataKey="name"
					width={110}
					tick={{ fontSize: 12 }}
				/>
				<Tooltip content={<CustomTooltip />} />
				{/* Reference lines at tier multiples */}
				{[1, 5, 10, 20].map((x) => (
					<ReferenceLine
						key={x}
						x={x}
						stroke="hsl(var(--muted-foreground))"
						strokeDasharray="4 4"
						strokeOpacity={0.4}
						label={{
							value: `${x}×`,
							fontSize: 9,
							fill: "hsl(var(--muted-foreground))",
						}}
					/>
				))}
				{/* lo range */}
				<Bar dataKey="lo" stackId="range" fill="transparent" />
				{/* hi-lo range bar */}
				<Bar dataKey={(d) => d.hi - d.lo} stackId="range" name="Range">
					{chartData.map((entry) => (
						<Cell
							key={entry.id}
							fill="hsl(var(--primary))"
							fillOpacity={confidenceOpacity(entry.nEff)}
						/>
					))}
				</Bar>
			</BarChart>
		</ResponsiveContainer>
	);
}

export function XFactorView() {
	const { data, isLoading, isError, error } = useXFactor();

	if (isLoading) {
		return (
			<div className="space-y-4">
				<div className="grid grid-cols-2 gap-4">
					{Array.from({ length: 2 }, (_, i) => `s${i}`).map((k) => (
						<Card key={k}>
							<CardContent className="p-4">
								<Skeleton className="h-16 w-full" />
							</CardContent>
						</Card>
					))}
				</div>
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
							Failed to load X-factor data.{" "}
							{error instanceof Error ? error.message : "Unknown error"}
						</p>
					</div>
				</CardContent>
			</Card>
		);
	}

	if (!data) return null;

	return (
		<div className="space-y-6">
			{/* Chart */}
			<Card>
				<CardHeader>
					<CardTitle className="text-base">X-Factor per Account</CardTitle>
					<p className="text-xs text-muted-foreground">
						Bars show lo–hi uncertainty range; width reflects estimation
						confidence (dimmer = less confident). Reference lines at 1×, 5×,
						10×, 20× tier multiples.
					</p>
				</CardHeader>
				<CardContent>
					{data.accounts.length === 0 ? (
						<p className="text-muted-foreground text-sm">
							No X-factor data yet.
						</p>
					) : (
						<XFactorChart accounts={data.accounts} />
					)}
				</CardContent>
			</Card>

			{/* Per-account table */}
			<Card>
				<CardHeader>
					<CardTitle className="text-base">Account Details</CardTitle>
				</CardHeader>
				<CardContent>
					<div className="overflow-x-auto">
						<table className="w-full text-sm">
							<thead>
								<tr className="border-b text-muted-foreground text-xs">
									<th className="text-left py-2 pr-4 font-medium">Account</th>
									<th className="text-right py-2 pr-4 font-medium">
										X-Factor (lo/mid/hi)
									</th>
									<th className="text-right py-2 pr-4 font-medium">
										Capacity 5h
									</th>
									<th className="text-right py-2 pr-4 font-medium">nEff</th>
									<th className="text-left py-2 font-medium">Status</th>
								</tr>
							</thead>
							<tbody>
								{data.accounts.map((acc) => (
									<tr
										key={acc.id}
										className="border-b last:border-0 hover:bg-muted/30 transition-colors"
									>
										<td className="py-2 pr-4 font-medium">{acc.name}</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.confidence === "cold" ? (
												<span className="text-muted-foreground">—</span>
											) : (
												<span
													title={`lo=${acc.xFactor.lo.toFixed(2)} mid=${acc.xFactor.mid.toFixed(2)} hi=${acc.xFactor.hi.toFixed(2)}`}
												>
													{acc.xFactor.lo.toFixed(1)}× /{" "}
													<strong>{acc.xFactor.mid.toFixed(1)}×</strong> /{" "}
													{acc.xFactor.hi.toFixed(1)}×
												</span>
											)}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.confidence === "cold" ? (
												<span className="text-muted-foreground">—</span>
											) : (
												<span
													title={`lo=${Math.round(acc.c5hTokens.lo / 1000)}K expected=${Math.round(acc.c5hTokens.mid / 1000)}K hi=${Math.round(acc.c5hTokens.hi / 1000)}K`}
												>
													{Math.round(acc.c5hTokens.mid / 1000)}K
												</span>
											)}
										</td>
										<td className="py-2 pr-4 text-right font-mono text-xs tabular-nums">
											{acc.nEff.toFixed(1)}
										</td>
										<td className="py-2">
											<div className="flex flex-wrap gap-1">
												<ConfidenceBadge
													confidence={acc.confidence}
													nEff={acc.nEff}
												/>
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
				</CardContent>
			</Card>
		</div>
	);
}

import { AlertCircle } from "lucide-react";
import { usePoolCapacity } from "../../hooks/capacity-queries";
import { Card, CardContent } from "../ui/card";
import { Skeleton } from "../ui/skeleton";
import { AccountCapacityCard } from "./AccountCapacityCard";
import { PoolSummaryBar } from "./PoolSummaryBar";

function PoolCapacitySkeleton() {
	return (
		<div className="space-y-6">
			<div className="grid grid-cols-2 md:grid-cols-4 gap-4">
				{Array.from({ length: 4 }, (_, i) => `skel-${i}`).map((k) => (
					<Card key={k}>
						<CardContent className="p-4">
							<Skeleton className="h-16 w-full" />
						</CardContent>
					</Card>
				))}
			</div>
			<div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
				{Array.from({ length: 3 }, (_, i) => `skel-acc-${i}`).map((k) => (
					<Card key={k}>
						<CardContent className="p-4">
							<Skeleton className="h-28 w-full" />
						</CardContent>
					</Card>
				))}
			</div>
		</div>
	);
}

export function PoolCapacityView() {
	const { data, isLoading, isError, error } = usePoolCapacity();

	if (isLoading) return <PoolCapacitySkeleton />;

	if (isError) {
		return (
			<Card className="border-destructive">
				<CardContent className="pt-6">
					<div className="flex items-center gap-2">
						<AlertCircle className="h-4 w-4 text-destructive" />
						<p className="text-destructive">
							Failed to load pool capacity.{" "}
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
			<PoolSummaryBar poolTotal={data.poolTotal} accounts={data.accounts} />
			{data.accounts.length === 0 ? (
				<p className="text-muted-foreground text-sm">
					No accounts with capacity data yet. Data appears after the first usage
					poll.
				</p>
			) : (
				<div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
					{data.accounts.map((account) => (
						<AccountCapacityCard key={account.id} account={account} />
					))}
				</div>
			)}
		</div>
	);
}

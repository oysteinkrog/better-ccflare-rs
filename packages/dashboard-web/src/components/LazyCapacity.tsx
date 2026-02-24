import { Suspense } from "react";
import { CapacityPage } from "./capacity/CapacityPage";
import { Card, CardContent } from "./ui/card";
import { Skeleton } from "./ui/skeleton";

const CapacitySkeleton = () => (
	<div className="space-y-6">
		<div className="grid grid-cols-2 md:grid-cols-4 gap-4">
			{Array.from({ length: 4 }, (_, i) => `sk-${i}`).map((k) => (
				<Card key={k}>
					<CardContent className="p-4">
						<Skeleton className="h-16 w-full" />
					</CardContent>
				</Card>
			))}
		</div>
		<div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
			{Array.from({ length: 3 }, (_, i) => `sk-acc-${i}`).map((k) => (
				<Card key={k}>
					<CardContent className="p-4">
						<Skeleton className="h-28 w-full" />
					</CardContent>
				</Card>
			))}
		</div>
	</div>
);

export const LazyCapacity = () => (
	<Suspense fallback={<CapacitySkeleton />}>
		<CapacityPage />
	</Suspense>
);

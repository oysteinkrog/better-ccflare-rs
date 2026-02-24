import { Tabs, TabsContent, TabsList, TabsTrigger } from "../ui/tabs";
import { PoolCapacityView } from "./PoolCapacityView";
import { ValueView } from "./ValueView";
import { XFactorView } from "./XFactorView";

export function CapacityPage() {
	return (
		<Tabs defaultValue="pool">
			<TabsList>
				<TabsTrigger value="pool">Pool</TabsTrigger>
				<TabsTrigger value="xfactor">X-Factor</TabsTrigger>
				<TabsTrigger value="value">Value & ROI</TabsTrigger>
			</TabsList>
			<TabsContent value="pool" className="mt-6">
				<PoolCapacityView />
			</TabsContent>
			<TabsContent value="xfactor" className="mt-6">
				<XFactorView />
			</TabsContent>
			<TabsContent value="value" className="mt-6">
				<ValueView />
			</TabsContent>
		</Tabs>
	);
}

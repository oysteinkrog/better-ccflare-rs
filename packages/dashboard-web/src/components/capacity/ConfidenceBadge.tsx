import type { ConfidenceLevel } from "../../types/capacity";
import { Badge } from "../ui/badge";

interface ConfidenceBadgeProps {
	confidence: ConfidenceLevel;
	nEff: number;
}

export function ConfidenceBadge({ confidence, nEff }: ConfidenceBadgeProps) {
	if (confidence === "high") return null; // absence = confidence

	const config = {
		cold: {
			label: "Cold",
			variant: "outline" as const,
			title: `No observations yet (nEff=${nEff.toFixed(1)})`,
		},
		low: {
			label: "Low conf",
			variant: "warning" as const,
			title: `Low confidence (nEff=${nEff.toFixed(1)}, need ≥4)`,
		},
		medium: {
			label: "Med conf",
			variant: "secondary" as const,
			title: `Medium confidence (nEff=${nEff.toFixed(1)}, need ≥10)`,
		},
	}[confidence];

	return (
		<Badge variant={config.variant} title={config.title} className="text-xs">
			{config.label}
		</Badge>
	);
}

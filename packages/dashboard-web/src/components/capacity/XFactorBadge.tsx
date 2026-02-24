import type { ConfidenceLevel, XFactorRange } from "../../types/capacity";
import { Badge } from "../ui/badge";

interface XFactorBadgeProps {
	xFactor: XFactorRange;
	confidence: ConfidenceLevel;
	nEff: number;
}

export function XFactorBadge({ xFactor, confidence, nEff }: XFactorBadgeProps) {
	const variant =
		confidence === "cold"
			? "outline"
			: confidence === "low"
				? "warning"
				: confidence === "medium"
					? "secondary"
					: "success";

	const label = confidence === "cold" ? "x?" : `x${xFactor.mid.toFixed(1)}`;

	const tooltip =
		confidence === "cold"
			? `X-factor unknown — no observations yet`
			: `X-factor: ${xFactor.lo.toFixed(1)}–${xFactor.mid.toFixed(1)}–${xFactor.hi.toFixed(1)} (lo/mid/hi) | Confidence: ${confidence} (nEff=${nEff.toFixed(1)})`;

	return (
		<Badge variant={variant} title={tooltip} className="font-mono text-xs">
			{label}
		</Badge>
	);
}

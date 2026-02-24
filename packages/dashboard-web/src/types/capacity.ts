// ── Pool Capacity (/api/analytics/pool-capacity) ──

export type ConfidenceLevel = "cold" | "low" | "medium" | "high";

export interface RemainingTokens {
	pessimistic: number;
	expected: number;
	optimistic: number;
}

export interface TteMinutes {
	current: number | null;
	withWindowRecovery: number | null;
}

export interface AccountCapacity {
	id: string;
	name: string;
	provider: string;
	remaining5h: RemainingTokens;
	remaining7d: {
		pessimistic: number;
		expected: number;
		optimistic: number | null;
	} | null;
	tteMinutes: TteMinutes;
	bindingConstraint: "5h" | "7d" | null;
	utilization5hPct: number;
	shouldStopAssign: boolean;
	lastPollAgeSeconds: number;
	nEff: number;
	confidence: ConfidenceLevel;
	externalTokens5h: number;
	kfExternalRateTokensPerSec: number;
	sharedScore: number;
	suspectedShared: boolean;
	windowRecovery15minTokens: number;
	isShared: boolean;
}

export interface PoolTotal {
	remainingPessimistic: number;
	remainingExpected: number;
	soonestTteMinutes: number | null;
	soonestTteWithRecoveryMinutes: number | null;
}

export interface PoolCapacityResponse {
	computedAt: string;
	accounts: AccountCapacity[];
	poolTotal: PoolTotal;
}

// ── X-Factor (/api/analytics/xfactor) ──

export interface XFactorRange {
	lo: number;
	mid: number;
	hi: number;
}

export interface XFactorAccount {
	id: string;
	name: string;
	subscriptionTier: string | null;
	xFactor: XFactorRange;
	c5hTokens: XFactorRange;
	nEff: number;
	confidence: ConfidenceLevel;
	suspectedShared: boolean;
	sharedScore: number;
	externalTokens5h: number;
	monthlyCostUsd: number | null;
	costSource: string;
	cpmActualUsd: number | null;
	cpmPaygEquivUsd: number | null;
	cpmTheoreticalUsd: number | null;
	realizedPct: number | null;
	recommendations: string[];
}

export interface XFactorResponse {
	computedAt: string;
	cBaseEstimate: number;
	cBaseSource: string;
	accounts: XFactorAccount[];
	poolSummary: {
		totalCapacity5h: { pessimistic: number; expected: number };
		totalRemaining5h: { pessimistic: number; expected: number };
		soonestExhaustionMinutes: number | null;
		recommendations: string[];
	};
}

// ── Per-Account X-Factor Detail (/api/accounts/:id/xfactor) ──

export interface AccountXFactorDetail {
	id: string;
	name: string;
	provider: string;
	xFactor: XFactorRange;
	c5hTokens: XFactorRange;
	nEff: number;
	confidence: ConfidenceLevel;
	suspectedShared: boolean;
	sharedScore: number;
	externalTokens5h: number;
	externalRateTokensPerSec: number;
	tteMinutes: TteMinutes;
	windowRecovery15minTokens: number;
}

// ── Value / ROI (/api/analytics/value) ──

export interface AccountValue {
	id: string;
	name: string;
	provider: string;
	subscriptionTier: string | null;
	monthlyCostUsd: number | null;
	costSource: string;
	rawTokens30d: number;
	paygCostUsd30d: number;
	requestCount30d: number;
	activeDays: number;
	cpmActualUsd: number | null;
	cpmPaygEquivUsd: number | null;
	cpmTheoreticalUsd: number | null;
	realizedPct: number | null;
	savingsUsd30d: number | null;
	recommendations: string[];
}

export interface ValueResponse {
	computedAt: string;
	accounts: AccountValue[];
	poolSummary: {
		totalSavingsUsd30d: number;
		totalPaygCostUsd30d: number;
		totalRawTokens30d: number;
		poolRealizedPct: number | null;
		recommendations: string[];
	};
}

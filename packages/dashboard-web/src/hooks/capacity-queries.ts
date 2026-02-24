import { useQuery } from "@tanstack/react-query";
import { api } from "../api";
import { queryKeys } from "../lib/query-keys";

export const usePoolCapacity = () => {
	return useQuery({
		queryKey: queryKeys.poolCapacity(),
		queryFn: () => api.getPoolCapacity(),
		staleTime: 15_000,
		refetchInterval: 30_000,
		refetchIntervalInBackground: false,
		gcTime: 5 * 60 * 1000,
	});
};

export const useXFactor = () => {
	return useQuery({
		queryKey: queryKeys.xfactor(),
		queryFn: () => api.getXFactor(),
		staleTime: 60_000,
		refetchInterval: 120_000,
		refetchIntervalInBackground: false,
		gcTime: 10 * 60 * 1000,
	});
};

export const useAccountXfactor = (accountId: string | null) => {
	return useQuery({
		queryKey: queryKeys.accountXfactor(accountId ?? ""),
		queryFn: () => api.getAccountXfactor(accountId!),
		enabled: !!accountId,
		staleTime: 60_000,
		gcTime: 5 * 60 * 1000,
	});
};

export const useValue = () => {
	return useQuery({
		queryKey: queryKeys.value(),
		queryFn: () => api.getValue(),
		staleTime: 120_000,
		refetchInterval: 300_000,
		refetchIntervalInBackground: false,
		gcTime: 30 * 60 * 1000,
	});
};

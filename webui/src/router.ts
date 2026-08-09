import { useCallback, useMemo } from "react";
import {
  useLocation as useWouterLocation,
  useSearch,
  type NavigateOptions,
} from "wouter";

export { Redirect, Route, Router, Switch, useSearchParams } from "wouter";

export interface AppLocation {
  pathname: string;
  search: string;
  hash: string;
  state: unknown;
}

export type NavigateFunction = {
  (to: string, options?: NavigateOptions): void;
  (delta: number): void;
};

export function useNavigate(): NavigateFunction {
  const [, navigate] = useWouterLocation();
  return useCallback(
    ((to: string | number, options?: NavigateOptions) => {
      if (typeof to === "number") {
        window.history.go(to);
        return;
      }
      navigate(to, options);
    }) as NavigateFunction,
    [navigate],
  );
}

export function useLocation(): AppLocation {
  const [pathname] = useWouterLocation();
  const search = useSearch();
  return useMemo(
    () => ({
      pathname,
      search,
      hash: window.location.hash,
      state: window.history.state,
    }),
    [pathname, search],
  );
}

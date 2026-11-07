import { useLayoutEffect, useRef } from "react";

export function usePreviewOwner() {
  const owner = useRef<AbortController | null>(null);
  useLayoutEffect(() => () => owner.current?.abort(), []);

  return () => {
    if (owner.current && !owner.current.signal.aborted) return null;
    const request = new AbortController();
    owner.current = request;
    return request;
  };
}

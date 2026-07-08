import { useState, useEffect } from "react";

let nowMs = Date.now();
let intervalId: ReturnType<typeof setInterval> | null = null;
const subscribers = new Set<React.Dispatch<React.SetStateAction<number>>>();

function ensureTimer() {
  if (intervalId !== null) return;
  intervalId = setInterval(() => {
    nowMs = Date.now();
    subscribers.forEach((fn) => fn(nowMs));
  }, 250);
}

function teardownTimerIfEmpty() {
  if (subscribers.size === 0 && intervalId !== null) {
    clearInterval(intervalId);
    intervalId = null;
  }
}

export function useSharedNowMs(active: boolean): number {
  const [ms, setMs] = useState(() => nowMs);

  useEffect(() => {
    if (!active) return;
    subscribers.add(setMs);
    ensureTimer();
    return () => {
      subscribers.delete(setMs);
      teardownTimerIfEmpty();
    };
  }, [active, setMs]);

  return ms;
}

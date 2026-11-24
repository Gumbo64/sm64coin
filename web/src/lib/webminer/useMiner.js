import { useSyncExternalStore } from 'react';
import { miner } from './controller.js';

/** The auto-miner controller's snapshot (settings, running, log, workers, stats, status), at most 4 updates a second. */
export function useMiner() {
  return useSyncExternalStore(miner.subscribe, miner.getSnapshot);
}

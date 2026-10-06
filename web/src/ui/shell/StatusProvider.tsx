import { createContext, type ComponentChildren } from 'preact';
import { useContext } from 'preact/hooks';
import { useServices } from '../../app/context';
import type { FeedStatus } from '../../data/indexer';
import { useFeedStatus, useIndexerStatus, useNodeStatus, useDeepReorgs, type DeepReorgs, type IndexerStatus, type NodeStatus } from './status';

export interface SystemStatus {
  indexer: IndexerStatus;
  node: NodeStatus;
  feed: FeedStatus;
  /** deep re-organisations seen since the page opened (shallow ones are routine and not tracked) */
  deepReorgs: DeepReorgs;
}

const StatusContext = createContext<SystemStatus | null>(null);

/** One shared poller for node + indexer health: the status bar and every view read the same values. */
export function StatusProvider(props: { children: ComponentChildren }) {
  const services = useServices();
  const indexer = useIndexerStatus(services);
  const node = useNodeStatus(services);
  const feed = useFeedStatus(services.feed);
  const deepReorgs = useDeepReorgs(services.feed);
  return <StatusContext.Provider value={{ indexer, node, feed, deepReorgs }}>{props.children}</StatusContext.Provider>;
}

export function useSystemStatus(): SystemStatus {
  const s = useContext(StatusContext);
  if (!s) throw new Error('useSystemStatus outside <StatusProvider>');
  return s;
}

import type { Milestone, Agreement } from '../lib/soroban';
import { getStatusBadgeColor, useMilestoneActions } from '../hooks/useMilestoneActions';

interface WalletLike {
  connected: boolean;
  publicKey: string | null;
  error: string | null;
}

interface MilestoneRowProps {
  milestone: Milestone;
  agreement: Agreement;
  wallet: WalletLike;
  onUpdate?: () => void;
}

/** Desktop table row — paired with `MilestoneCard` for the mobile layout. */
export default function MilestoneRow({ milestone, agreement, wallet }: MilestoneRowProps) {
  const {
    availableActions,
    actionLoading,
    actionError,
    showProofInput,
    setShowProofInput,
    proofUri,
    setProofUri,
    isUserPayee,
  } = useMilestoneActions(milestone, agreement, wallet, onUpdate);

  return (
    <tr className="border-b border-navy-700 dark:border-navy-700 light:border-gray-200 hover:bg-navy-700/50 dark:hover:bg-navy-700/50 light:hover:bg-gray-100">
      <td className="py-3 px-4 text-white dark:text-white light:text-gray-900 font-mono text-sm">{milestone.id}</td>
      <td className="py-3 px-4 text-white dark:text-white light:text-gray-900 text-sm">{milestone.amount}</td>
      <td className="py-3 px-4">
        <span
          className={`${getStatusBadgeColor(
            milestone.status
          )} text-white dark:text-white light:text-gray-900 text-xs font-semibold px-2 py-1 rounded`}
        >
          {milestone.status}
        </span>
      </td>
      <td className="py-3 px-4">
        {milestone.proof_uri ? (
          <a
            href={milestone.proof_uri}
            target="_blank"
            rel="noopener noreferrer"
            className="text-cyan-400 dark:text-cyan-400 light:text-cyan-600 hover:text-cyan-300 text-sm"
          >
            View
          </a>
        ) : (
          <span className="text-gray-500 dark:text-gray-500 light:text-gray-600 text-sm">-</span>
        )}
      </td>
      <td className="py-3 px-4">
        <div className="flex gap-2 flex-wrap">
          {availableActions.length > 0 ? (
            availableActions.map((action) => (
              <button
                key={action.label}
                onClick={action.action}
                disabled={actionLoading || (action.requiresWallet && !wallet.connected)}
                className="px-3 py-1 bg-cyan-500 hover:bg-cyan-400 text-navy-900 font-semibold text-xs rounded transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {action.label}
              </button>
            ))
          ) : (
            <span className="text-gray-500 dark:text-gray-500 light:text-gray-600 text-sm">-</span>
          )}
        </div>

        {milestone.status === 'Funded' && isUserPayee && showProofInput && (
          <div className="mt-2 space-y-2">
            <input
              type="text"
              placeholder="Proof URI (e.g., GitHub PR link)"
              value={proofUri}
              onChange={(e) => setProofUri(e.target.value)}
              className="w-full px-3 py-2 bg-navy-700 dark:bg-navy-700 light:bg-gray-100 border border-navy-600 dark:border-navy-600 light:border-gray-300 text-white dark:text-white light:text-gray-900 text-xs rounded focus:outline-none focus:border-cyan-400"
            />
            <button
              onClick={() => {
                setShowProofInput(false);
                setProofUri('');
              }}
              className="text-xs text-gray-400 dark:text-gray-400 light:text-gray-600 hover:text-gray-300 dark:hover:text-gray-300 light:hover:text-gray-700"
            >
              Cancel
            </button>
          </div>
        )}

        {actionError && <p className="mt-2 text-red-400 text-xs">{actionError}</p>}
      </td>
    </tr>
  );
}

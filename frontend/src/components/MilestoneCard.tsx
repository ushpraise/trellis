import type { Milestone, Agreement } from '../lib/soroban';
import { getStatusBadgeColor, useMilestoneActions } from '../hooks/useMilestoneActions';

interface WalletLike {
  connected: boolean;
  publicKey: string | null;
  loading: boolean;
  error: string | null;
}

interface MilestoneCardProps {
  milestone: Milestone;
  agreement: Agreement;
  wallet: WalletLike;
  onUpdate?: () => void;
}

/** Mobile single-column card — paired with `MilestoneRow` for the desktop table. */
export default function MilestoneCard({ milestone, agreement, wallet, onUpdate }: MilestoneCardProps) {
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
    <div className="rounded-lg border border-navy-700 bg-navy-800/60 p-4">
      <div className="flex items-center justify-between">
        <span className="font-mono text-sm text-gray-400">Milestone {milestone.id}</span>
        <span
          className={`${getStatusBadgeColor(
            milestone.status
          )} text-white text-xs font-semibold px-2 py-1 rounded`}
        >
          {milestone.status}
        </span>
      </div>

      <p className="mt-2 text-lg font-semibold text-white">{milestone.amount}</p>

      <div className="mt-1">
        {milestone.proof_uri ? (
          <a
            href={milestone.proof_uri}
            target="_blank"
            rel="noopener noreferrer"
            className="text-cyan-400 hover:text-cyan-300 text-sm"
          >
            View proof
          </a>
        ) : (
          <span className="text-gray-500 text-sm">No proof submitted</span>
        )}
      </div>

      {availableActions.length > 0 && (
        <div className="mt-3 flex flex-col gap-2">
          {availableActions.map((action) => (
            <button
              key={action.label}
              onClick={action.action}
              disabled={actionLoading || (action.requiresWallet && !wallet.connected)}
              className="min-h-[44px] px-4 py-2 bg-cyan-500 hover:bg-cyan-400 text-navy-900 font-semibold text-sm rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {action.label}
            </button>
          ))}
        </div>
      )}

      {milestone.status === 'Funded' && isUserPayee && showProofInput && (
        <div className="mt-3 space-y-2">
          <input
            type="text"
            placeholder="Proof URI (e.g., GitHub PR link)"
            value={proofUri}
            onChange={(e) => setProofUri(e.target.value)}
            className="min-h-[44px] w-full px-3 py-2 bg-navy-700 border border-navy-600 text-white text-sm rounded-lg focus:outline-none focus:border-cyan-400"
          />
          <button
            onClick={() => {
              setShowProofInput(false);
              setProofUri('');
            }}
            className="min-h-[44px] px-4 text-sm text-gray-400 hover:text-gray-300"
          >
            Cancel
          </button>
        </div>
      )}

      {actionError && <p className="mt-2 text-red-400 text-xs">{actionError}</p>}
    </div>
  );
}

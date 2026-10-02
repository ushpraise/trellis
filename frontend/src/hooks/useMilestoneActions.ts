import { useState } from 'react';
import { nativeToScVal, type xdr } from '@stellar/stellar-sdk';
import { useContractInvoke } from './useContractInvoke';
import { proofUriToScVal } from '../components/MilestoneActions';
import { hexToBytes } from '../lib/format';
import type { Milestone, Agreement, EscrowStatus } from '../lib/soroban';

interface WalletLike {
  connected: boolean;
  publicKey: string | null;
  loading: boolean;
  error: string | null;
}

const STATUS_BADGE_COLORS: Record<EscrowStatus, string> = {
  Pending: 'bg-gray-600',
  Funded: 'bg-blue-600',
  WorkSubmitted: 'bg-yellow-600',
  Completed: 'bg-green-600',
  Disputed: 'bg-red-600',
  Refunded: 'bg-gray-600',
  Cancelled: 'bg-slate-500',
};

export function getStatusBadgeColor(status: EscrowStatus): string {
  return STATUS_BADGE_COLORS[status];
}

/**
 * Shared action/state logic behind both the desktop table row
 * (`MilestoneRow`) and the mobile card (`MilestoneCard`) for a single
 * milestone, so the two layouts never drift out of sync on what actions
 * are available or how they behave.
 */
export function useMilestoneActions(
  milestone: Milestone,
  agreement: Agreement,
  wallet: WalletLike,
  onSuccess?: () => void,
) {
  const { invoke } = useContractInvoke();
  const [actionLoading, setActionLoading] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [showProofInput, setShowProofInput] = useState(false);
  const [proofUri, setProofUri] = useState('');

  const isUserPayer = wallet.publicKey === agreement.payer;
  const isUserPayee = wallet.publicKey === agreement.payee;

  // lock_funds, submit_work and approve_and_release take no caller argument — the
  // contract derives the signer from agreement.payer / agreement.payee. Only
  // raise_dispute takes an explicit leading `caller`.
  const runAction = async (
    method: string,
    buildArgs: () => xdr.ScVal[],
    failureMessage: string,
    onDone?: () => void,
  ) => {
    if (!wallet.connected || !wallet.publicKey) {
      setActionError('Please connect your wallet');
      return;
    }

    setActionLoading(true);
    setActionError(null);

    try {
      await invoke(method, buildArgs(), wallet.publicKey);
      onDone?.();
      onSuccess?.();
    } catch (error) {
      setActionError(error instanceof Error ? error.message : failureMessage);
    } finally {
      setActionLoading(false);
    }
  };

  const idArgs = () => [
    nativeToScVal(hexToBytes(agreement.agreement_id), { type: 'bytes' }),
    nativeToScVal(milestone.id, { type: 'u32' }),
  ];

  const handleLockFunds = () => runAction('lock_funds', idArgs, 'Failed to lock funds');

  const handleSubmitWork = () => {
    // First click reveals the proof input; the next click submits.
    if (!showProofInput) {
      setShowProofInput(true);
      return Promise.resolve();
    }
    if (!proofUri.trim()) {
      setActionError('Please enter a proof URI');
      return Promise.resolve();
    }
    return runAction('submit_work', () => [...idArgs(), proofUriToScVal(proofUri)], 'Failed to submit work', () => {
      setShowProofInput(false);
      setProofUri('');
    });
  };

  const handleApproveRelease = () =>
    runAction('approve_and_release', idArgs, 'Failed to approve and release');

  const handleRaiseDispute = () =>
    runAction(
      'raise_dispute',
      () => [nativeToScVal(wallet.publicKey, { type: 'address' }), ...idArgs()],
      'Failed to raise dispute',
    );

  const availableActions: Array<{ label: string; action: () => void; requiresWallet?: boolean }> = [];

  if (milestone.status === 'Pending' && isUserPayer) {
    availableActions.push({ label: 'Lock Funds', action: handleLockFunds, requiresWallet: true });
  }

  if (milestone.status === 'Funded' && isUserPayee) {
    availableActions.push({ label: 'Submit Work', action: handleSubmitWork, requiresWallet: true });
  }

  if (milestone.status === 'WorkSubmitted' && isUserPayer) {
    availableActions.push({ label: 'Approve & Release', action: handleApproveRelease, requiresWallet: true });
  }

  // raise_dispute rejects any caller other than the payer or payee.
  if ((milestone.status === 'Funded' || milestone.status === 'WorkSubmitted') && wallet.connected && (isUserPayer || isUserPayee)) {
    availableActions.push({ label: 'Raise Dispute', action: handleRaiseDispute, requiresWallet: true });
  }

  return {
    availableActions,
    actionLoading,
    actionError,
    showProofInput,
    setShowProofInput,
    proofUri,
    setProofUri,
    isUserPayee,
  };
}

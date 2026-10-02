import { useState, useCallback } from 'react'
import QRCode from 'qrcode'
import { generateAgreementId } from '../lib/agreementId'
import { TruncatedAddress } from './TruncatedAddress'
import useToast from '../hooks/useToast'

interface AgreementIdGeneratorProps {
  /** Called when a new ID is generated so the parent form can consume it. */
  onGenerate?: (id: string) => void
}

/** Detects canvas element support (unavailable during SSR or in very old browsers). */
function isCanvasSupported(): boolean {
  return typeof document !== 'undefined' && typeof HTMLCanvasElement !== 'undefined'
}

export function AgreementIdGenerator({ onGenerate }: AgreementIdGeneratorProps) {
  const toast = useToast()
  const [agreementId, setAgreementId] = useState<string>('')
  const [qrDataUrl, setQrDataUrl] = useState<string>('')
  const [qrUnavailable, setQrUnavailable] = useState(false)
  const [showQR, setShowQR] = useState(false)
  const [copied, setCopied] = useState(false)
  const [shared, setShared] = useState(false)
  const [isGenerating, setIsGenerating] = useState(false)

  const handleGenerate = useCallback(async () => {
    setIsGenerating(true)
    const newId = generateAgreementId()
    setAgreementId(newId)
    setQrDataUrl('')
    setQrUnavailable(false)

    // Generate QR code locally via the qrcode library, when canvas is available
    if (isCanvasSupported()) {
      try {
        const url = await QRCode.toDataURL(newId, {
          width: 400,
          margin: 2,
          color: {
            dark: '#1a2332',
            light: '#ffffff',
          },
        })
        setQrDataUrl(url)
      } catch {
        // Local QR generation failed; fall back to the text-only display
        setQrUnavailable(true)
      }
    } else {
      setQrUnavailable(true)
    }

    setShowQR(true)
    setCopied(false)
    setShared(false)
    setIsGenerating(false)
    onGenerate?.(newId)
  }, [onGenerate])

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(agreementId)
      setCopied(true)
      setTimeout(() => setCopied(false), 2000)
    } catch {
      toast.error({ title: 'Copy failed', message: 'Could not access clipboard. Check browser permissions.' })
    }
  }, [agreementId, toast])

  const handleShare = useCallback(async () => {
    if (navigator.share) {
      try {
        await navigator.share({
          title: 'Trellis Agreement ID',
          text: `Agreement ID: ${agreementId}\n\nView on Trellis: ${window.location.origin}/agreement/${agreementId}`,
        })
        setShared(true)
        setTimeout(() => setShared(false), 2000)
      } catch (err) {
        // User cancelled the share sheet (AbortError) — intentional, stay silent
        if (err instanceof DOMException && err.name === 'AbortError') return
        toast.error({ title: 'Share failed', message: 'Could not open the share dialog. Try copying the ID instead.' })
      }
    } else {
      // Web Share API unavailable — copy to clipboard instead
      await handleCopy()
    }
  }, [agreementId, handleCopy, toast])

  return (
    <div className="w-full max-w-md mx-auto p-4 sm:p-6 bg-navy-800 dark:bg-navy-800 light:bg-gray-50 rounded-lg border border-navy-700 dark:border-navy-700 light:border-gray-200 shadow-lg">
      <div className="flex items-center gap-2 mb-4">
        <svg
          className="w-5 h-5 text-cyan-400 shrink-0"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
        >
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
          <rect x="8" y="8" width="8" height="8" rx="1" />
        </svg>
        <h2 className="text-xl sm:text-2xl font-bold text-white dark:text-white light:text-gray-900">
          Agreement ID
        </h2>
      </div>
      <p className="text-gray-400 dark:text-gray-400 light:text-gray-600 text-sm mb-6">
        Create a cryptographically random 64-character hex ID to share between payer and payee.
      </p>

      <button
        onClick={handleGenerate}
        disabled={isGenerating}
        className="group relative w-full bg-cyan-400 text-navy-900 font-semibold px-4 py-2.5 rounded-lg text-sm sm:text-base transition-all duration-200 hover:bg-cyan-300 hover:shadow-lg hover:shadow-cyan-400/20 active:scale-[0.98] disabled:opacity-60 disabled:cursor-not-allowed disabled:hover:shadow-none"
      >
        <span className={`inline-flex items-center gap-2 ${isGenerating ? 'opacity-0' : ''}`}>
          <svg
            className="w-4 h-4"
            fill="none"
            viewBox="0 0 24 24"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
            <polyline points="14 2 14 8 20 8" />
            <line x1="12" y1="18" x2="12" y2="12" />
            <line x1="9" y1="15" x2="15" y2="15" />
          </svg>
          Generate New ID
        </span>
        {isGenerating && (
          <span className="absolute inset-0 flex items-center justify-center">
            <svg
              className="animate-spin h-5 w-5 text-navy-900"
              fill="none"
              viewBox="0 0 24 24"
            >
              <circle
                className="opacity-25"
                cx="12"
                cy="12"
                r="10"
                stroke="currentColor"
                strokeWidth="4"
              />
              <path
                className="opacity-75"
                fill="currentColor"
                d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
              />
            </svg>
          </span>
        )}
      </button>

      {agreementId && (
        <div className="mt-6 space-y-4 animate-fade-in">
          {/* ID display */}
          <div className="p-3 bg-navy-700 dark:bg-navy-700 light:bg-gray-100 rounded-lg border border-navy-600 dark:border-navy-600 light:border-gray-300">
            <TruncatedAddress
              address={agreementId}
              chars={16}
              label="Agreement ID"
            />
          </div>

          {/* QR Code */}
          {showQR && qrDataUrl && (
            <div className="p-4 bg-white rounded-lg flex justify-center transition-all duration-300">
              <img
                src={qrDataUrl}
                alt="Agreement ID QR Code"
                className="w-48 h-48"
              />
            </div>
          )}

          {/* Text-only fallback when the QR code cannot be rendered (no canvas support) */}
          {showQR && qrUnavailable && (
            <div className="p-4 bg-navy-700 dark:bg-navy-700 light:bg-gray-100 rounded-lg border border-navy-600 dark:border-navy-600 light:border-gray-300 text-center space-y-2 transition-all duration-300">
              <p className="text-xs text-gray-400 dark:text-gray-400 light:text-gray-600">
                QR code isn't available in this browser. Use the ID below instead.
              </p>
              <p className="font-mono text-sm text-cyan-300 break-all leading-relaxed">
                {agreementId.match(/.{1,8}/g)?.join(' ') ?? agreementId}
              </p>
            </div>
          )}

          {/* Action buttons */}
          <div className="flex gap-2">
            <button
              onClick={handleCopy}
              className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-2.5 bg-navy-600 hover:bg-navy-500 text-gray-200 rounded-lg transition-all duration-200 text-sm font-medium active:scale-[0.98]"
            >
              {copied ? (
                <>
                  <svg className="w-4 h-4 text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="20 6 9 17 4 12" />
                  </svg>
                  <span className="text-green-400">Copied!</span>
                </>
              ) : (
                <>
                  <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
                    <path d="M5 15H4a2 2 0 01-2-2V4a2 2 0 012-2h9a2 2 0 012 2v1" />
                  </svg>
                  Copy
                </>
              )}
            </button>
            <button
              onClick={handleShare}
              className="flex-1 inline-flex items-center justify-center gap-2 px-4 py-2.5 bg-navy-600 hover:bg-navy-500 text-gray-200 rounded-lg transition-all duration-200 text-sm font-medium active:scale-[0.98]"
            >
              {shared ? (
                <>
                  <svg className="w-4 h-4 text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <polyline points="20 6 9 17 4 12" />
                  </svg>
                  <span className="text-green-400">Shared!</span>
                </>
              ) : (
                <>
                  <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <circle cx="18" cy="5" r="3" />
                    <circle cx="6" cy="12" r="3" />
                    <circle cx="18" cy="19" r="3" />
                    <line x1="8.59" y1="13.51" x2="15.42" y2="17.49" />
                    <line x1="15.41" y1="6.51" x2="8.59" y2="10.49" />
                  </svg>
                  Share
                </>
              )}
            </button>
          </div>

          <p className="text-xs text-gray-500 dark:text-gray-500 light:text-gray-600 text-center">
            Share this ID or QR code with your counterparty to begin an agreement.
          </p>
        </div>
      )}
    </div>
  )
}

export default AgreementIdGenerator

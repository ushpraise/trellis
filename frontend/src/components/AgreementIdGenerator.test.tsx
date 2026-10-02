import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { render, screen, fireEvent, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { AgreementIdGenerator } from './AgreementIdGenerator'
import ToastProvider from './toast/ToastProvider'

// Mock the qrcode library since it depends on Canvas which isn't available in jsdom
vi.mock('qrcode', () => ({
  toDataURL: vi.fn().mockResolvedValue('data:image/png;base64,mocked-qr-code'),
}))

// Mock clipboard API
Object.assign(navigator, {
  clipboard: {
    writeText: vi.fn().mockResolvedValue(undefined),
  },
  share: vi.fn().mockResolvedValue(undefined),
})

describe('AgreementIdGenerator', () => {
  beforeEach(() => {
    vi.clearAllMocks()
  })

  it('renders the generator heading and description', () => {
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)
    expect(screen.getByText('Agreement ID')).toBeInTheDocument()
    expect(
      screen.getByText(/cryptographically random 64-character hex ID/),
    ).toBeInTheDocument()
  })

  it('renders a "Generate New ID" button', () => {
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)
    expect(
      screen.getByRole('button', { name: /generate new id/i }),
    ).toBeInTheDocument()
  })

  it('generates an ID and shows QR code when button is clicked', async () => {
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    const generateBtn = screen.getByRole('button', { name: /generate new id/i })
    await user.click(generateBtn)

    // Wait for the QR code image to appear
    await waitFor(() => {
      expect(screen.getByAltText('Agreement ID QR Code')).toBeInTheDocument()
    })
  })

  it('shows Copy and Share buttons after generating an ID', async () => {
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /copy/i })).toBeInTheDocument()
      expect(screen.getByRole('button', { name: /share/i })).toBeInTheDocument()
    })
  })

  it('copies the agreement ID to clipboard when Copy is clicked', async () => {
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /copy/i })).toBeInTheDocument()
    })

    const copyBtn = screen.getByRole('button', { name: /copy/i })
    await user.click(copyBtn)

    expect(navigator.clipboard.writeText).toHaveBeenCalledOnce()
    const copiedValue = (navigator.clipboard.writeText as ReturnType<typeof vi.fn>).mock
      .calls[0][0]
    expect(copiedValue).toHaveLength(64)
    expect(copiedValue).toMatch(/^[0-9a-f]{64}$/)
  })

  it('calls onGenerate callback with the generated ID', async () => {
    const onGenerate = vi.fn()
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator onGenerate={onGenerate} /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))

    await waitFor(() => {
      expect(onGenerate).toHaveBeenCalledOnce()
    })

    const generatedId = onGenerate.mock.calls[0][0]
    expect(generatedId).toHaveLength(64)
    expect(generatedId).toMatch(/^[0-9a-f]{64}$/)
  })

  it('shows "Copied!" state temporarily after copying', async () => {
    vi.useFakeTimers()
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /^copy$/i })).toBeInTheDocument()
    })

    await user.click(screen.getByRole('button', { name: /^copy$/i }))

    await waitFor(() => {
      expect(screen.getByText('Copied!')).toBeInTheDocument()
    })

    vi.useRealTimers()
  })

  it('uses Web Share API when available', async () => {
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))

    await waitFor(() => {
      expect(screen.getByRole('button', { name: /^share$/i })).toBeInTheDocument()
    })

    await user.click(screen.getByRole('button', { name: /^share$/i }))

    expect(navigator.share).toHaveBeenCalledOnce()
    const shareData = (navigator.share as ReturnType<typeof vi.fn>).mock.calls[0][0]
    expect(shareData.title).toBe('Trellis Agreement ID')
    expect(shareData.text).toContain('Agreement ID:')
  })

  it('shows an error toast when sharing fails for a reason other than cancellation', async () => {
    vi.mocked(navigator.share).mockRejectedValueOnce(new DOMException('Denied', 'NotAllowedError'))
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))
    await user.click(await screen.findByRole('button', { name: /^share$/i }))

    expect(await screen.findByText('Share failed')).toBeInTheDocument()
  })

  it('stays silent when the user cancels the share sheet', async () => {
    vi.mocked(navigator.share).mockRejectedValueOnce(new DOMException('Cancelled', 'AbortError'))
    const user = userEvent.setup()
    render(<ToastProvider><AgreementIdGenerator /></ToastProvider>)

    await user.click(screen.getByRole('button', { name: /generate new id/i }))
    await user.click(await screen.findByRole('button', { name: /^share$/i }))

    await waitFor(() => expect(navigator.share).toHaveBeenCalledOnce())
    expect(screen.queryByText('Share failed')).not.toBeInTheDocument()
  })
})

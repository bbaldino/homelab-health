import { useEffect, useRef } from "preact/hooks";
import type { ComponentChildren } from "preact";

interface ModalProps {
  title: string;
  onClose: () => void;
  children: ComponentChildren;
}

/** A minimal accessible modal: overlay, Escape-to-close, click-outside-to-close, initial focus. */
export function Modal({ title, onClose, children }: ModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    const focusable = dialogRef.current?.querySelector<HTMLElement>(
      "input, select, textarea, button:not([disabled])",
    );
    (focusable ?? dialogRef.current)?.focus();

    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
      }
    }
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      previouslyFocused?.focus();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  return (
    <div
      class="modal-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        class="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="modal-title"
        tabIndex={-1}
        ref={dialogRef}
      >
        <div class="modal-header">
          <h2 id="modal-title">{title}</h2>
          <button
            type="button"
            class="modal-close"
            onClick={onClose}
            aria-label="Close"
          >
            ×
          </button>
        </div>
        <div class="modal-body">{children}</div>
      </div>
    </div>
  );
}

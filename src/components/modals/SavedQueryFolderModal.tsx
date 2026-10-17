import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Folder, X } from "lucide-react";
import { Modal } from "../ui/Modal";
import type { SavedQueryFolder } from "../../contexts/SavedQueriesContext";
import {
  MAX_FOLDER_NAME_CHARS,
  getFolderNameError,
} from "../../utils/savedQueryFolders";
import { toErrorMessage } from "../../utils/errors";

interface SavedQueryFolderModalProps {
  isOpen: boolean;
  onClose: () => void;
  folders: SavedQueryFolder[];
  /** The folder being renamed; omit to create a new one. */
  folder?: SavedQueryFolder | null;
  onSubmit: (name: string) => Promise<void>;
}

export const SavedQueryFolderModal = ({
  isOpen,
  onClose,
  folders,
  folder = null,
  onSubmit,
}: SavedQueryFolderModalProps) => {
  const { t } = useTranslation();
  const titleId = useId();
  const nameId = useId();
  const errorId = useId();
  const [name, setName] = useState("");
  const [touched, setTouched] = useState(false);
  const [serverError, setServerError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setName(folder?.name ?? "");
      setTouched(false);
      setServerError("");
    }
  }, [isOpen, folder]);

  const nameError = getFolderNameError(name, folders, folder?.id);
  const errorText =
    nameError === "empty"
      ? t("sidebar.folderNameEmpty")
      : nameError === "tooLong"
        ? t("sidebar.folderNameTooLong", { max: MAX_FOLDER_NAME_CHARS })
        : nameError === "duplicate"
          ? t("sidebar.folderNameDuplicate")
          : "";
  const visibleError = serverError || (touched ? errorText : "");

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setTouched(true);
    if (nameError) return;
    setBusy(true);
    try {
      await onSubmit(name.trim());
      onClose();
    } catch (err) {
      setServerError(toErrorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  const title = folder ? t("sidebar.renameFolder") : t("sidebar.newFolder");

  return (
    <Modal isOpen={isOpen} onClose={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="bg-elevated border border-strong rounded-xl shadow-2xl w-[420px] max-w-[90vw] overflow-hidden flex flex-col"
      >
        <div className="flex items-center justify-between p-4 border-b border-default bg-base">
          <div className="flex items-center gap-3">
            <div className="p-2 bg-accent-primary/15 rounded-lg">
              <Folder size={20} className="text-accent" />
            </div>
            <h2 id={titleId} className="text-lg font-semibold text-primary">
              {title}
            </h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label={t("common.cancel")}
            className="text-secondary hover:text-primary transition-colors rounded focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
          >
            <X size={20} />
          </button>
        </div>

        <form onSubmit={handleSubmit} className="p-4 space-y-4">
          <div>
            <label htmlFor={nameId} className="block text-sm font-medium text-secondary mb-1">
              {t("sidebar.folderName")}
            </label>
            <input
              id={nameId}
              autoCorrect="off"
              autoCapitalize="off"
              autoComplete="off"
              spellCheck={false}
              type="text"
              autoFocus
              value={name}
              onChange={(e) => {
                setName(e.target.value);
                setServerError("");
              }}
              onBlur={() => setTouched(true)}
              placeholder={t("sidebar.folderNamePlaceholder")}
              aria-invalid={visibleError ? true : undefined}
              aria-describedby={visibleError ? errorId : undefined}
              className="w-full bg-base border border-strong rounded px-3 py-2 text-primary placeholder:text-muted focus:outline-none focus:border-focus focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
            />
            {visibleError && (
              <div id={errorId} role="alert" className="text-accent-error text-xs mt-1">
                {visibleError}
              </div>
            )}
          </div>

          <div className="flex justify-end gap-2">
            <button
              type="button"
              onClick={onClose}
              className="px-4 py-2 text-secondary hover:text-primary hover:bg-surface-secondary rounded transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
            >
              {t("common.cancel")}
            </button>
            <button
              type="submit"
              disabled={busy}
              className="px-4 py-2 bg-accent-primary hover:bg-accent-primary/90 text-inverse rounded font-medium disabled:opacity-50 transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
            >
              {folder ? t("sidebar.renameFolderAction") : t("sidebar.createFolderAction")}
            </button>
          </div>
        </form>
      </div>
    </Modal>
  );
};

import { useEffect, useId, useState } from "react";
import { useTranslation } from "react-i18next";
import { Folder, X } from "lucide-react";
import { Modal } from "../ui/Modal";
import { Select } from "../ui/Select";
import type { SavedQuery, SavedQueryFolder } from "../../contexts/SavedQueriesContext";
import { UNFILED_OPTION } from "../../utils/savedQueryFolders";
import { toErrorMessage } from "../../utils/errors";

interface MoveSavedQueryModalProps {
  isOpen: boolean;
  onClose: () => void;
  query: SavedQuery | null;
  folders: SavedQueryFolder[];
  /** A null folderId moves the query to Unfiled. */
  onMove: (queryId: string, folderId: string | null) => Promise<void>;
}

export const MoveSavedQueryModal = ({
  isOpen,
  onClose,
  query,
  folders,
  onMove,
}: MoveSavedQueryModalProps) => {
  const { t } = useTranslation();
  const titleId = useId();
  const selectId = useId();
  const errorId = useId();
  const [target, setTarget] = useState<string>(UNFILED_OPTION);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    if (isOpen) {
      setTarget(query?.folder_id ?? UNFILED_OPTION);
      setError("");
    }
  }, [isOpen, query]);

  if (!query) return null;

  const options = [UNFILED_OPTION, ...folders.map((f) => f.id)];
  const labels: Record<string, string> = {
    [UNFILED_OPTION]: t("sidebar.unfiledFolderOption"),
    ...Object.fromEntries(folders.map((f) => [f.id, f.name])),
  };
  const current = query.folder_id ?? UNFILED_OPTION;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      await onMove(query.id, target === UNFILED_OPTION ? null : target);
      onClose();
    } catch (err) {
      setError(toErrorMessage(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal isOpen={isOpen} onClose={onClose}>
      <div
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        className="bg-elevated border border-strong rounded-xl shadow-2xl w-[420px] max-w-[90vw] flex flex-col"
      >
        <div className="flex items-center justify-between p-4 border-b border-default bg-base rounded-t-xl">
          <div className="flex items-center gap-3 min-w-0">
            <div className="p-2 bg-accent-primary/15 rounded-lg shrink-0">
              <Folder size={20} className="text-accent" />
            </div>
            <h2 id={titleId} className="text-lg font-semibold text-primary truncate">
              {t("sidebar.moveToFolderTitle", { name: query.name })}
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
            <label htmlFor={selectId} className="block text-sm font-medium text-secondary mb-1">
              {t("sidebar.folderField")}
            </label>
            <Select
              id={selectId}
              value={target}
              options={options}
              labels={labels}
              onChange={(value) => {
                setTarget(value);
                setError("");
              }}
              searchable={options.length > 8}
              ariaDescribedBy={error ? errorId : undefined}
            />
            {folders.length === 0 && (
              <p className="text-xs text-muted mt-2">{t("sidebar.noFoldersYet")}</p>
            )}
            {error && (
              <div id={errorId} role="alert" className="text-accent-error text-xs mt-2">
                {error}
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
              disabled={busy || target === current}
              className="px-4 py-2 bg-accent-primary hover:bg-accent-primary/90 text-inverse rounded font-medium disabled:opacity-50 transition-colors focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus"
            >
              {t("sidebar.moveAction")}
            </button>
          </div>
        </form>
      </div>
    </Modal>
  );
};

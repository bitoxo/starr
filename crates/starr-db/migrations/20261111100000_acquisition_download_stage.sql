-- Download stage of an acquisition (maintainer's acceptance test,
-- 2026-11-02): "Downloading 100%" for a title the source imported long
-- ago reads as stuck.
--
-- `download_stage` is live state like `progress`, overwritten by
-- `acquisitions.sync` (`starr_core::requests::observe`): `importing` while
-- the source's queue holds only completed downloads waiting for or in the
-- import, `arriving` when the source has the files but the category's
-- consume services do not list them yet. It only means something while the
-- acquisition is `downloading`; readers ignore it in every other state
-- (`DownloadStage::of`), so the state CHECK and the table stay as they are.

-- `download_stage_since` is when the current stage began: `acquisitions.sync`
-- asks for an early `inventory.sync` only for titles that started arriving
-- recently, so a media server that never lists a title is not walked every
-- few minutes forever.

ALTER TABLE acquisition ADD COLUMN download_stage TEXT
    CHECK (download_stage IS NULL OR download_stage IN ('importing', 'arriving'));
ALTER TABLE acquisition ADD COLUMN download_stage_since TEXT;

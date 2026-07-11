import { invoke } from '@tauri-apps/api/core';
import type { EffectLevel, IndexStats, IndexStatus, PreviewResult, SearchHit } from './types';

export function indexStatus(): Promise<IndexStatus> {
	return invoke('index_status');
}

export function search(query: string, limit = 30): Promise<SearchHit[]> {
	return invoke('search', { query, limit });
}

export function preview(path: string, query: string): Promise<PreviewResult | null> {
	return invoke('preview', { path, query });
}

export function openFile(path: string): Promise<void> {
	return invoke('open_file', { path });
}

export function revealInFolder(path: string): Promise<void> {
	return invoke('reveal_in_folder', { path });
}

export function rebuildIndex(dir: string): Promise<IndexStats> {
	return invoke('rebuild_index', { dir });
}

export function getEffectLevel(): Promise<EffectLevel> {
	return invoke('get_effect_level');
}

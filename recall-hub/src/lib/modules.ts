export interface HubModule { id: string; title: string; icon: 'clips' | 'editor' | 'hdr' | 'audio'; route: string; }
export const MODULES: HubModule[] = [{ id:'clips',title:'Clips',icon:'clips',route:'/'},{id:'editor',title:'Editor',icon:'editor',route:'/editor'},{id:'hdr',title:'HDR',icon:'hdr',route:'/hdr'},{id:'audio',title:'Audio',icon:'audio',route:'/audio'}];

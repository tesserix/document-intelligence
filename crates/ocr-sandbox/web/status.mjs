export const isFailedJob=state=>['rejected','failed','cancelled'].includes(state);
export const isTerminalJob=state=>isFailedJob(state)||['completed','partial','review_required'].includes(state);

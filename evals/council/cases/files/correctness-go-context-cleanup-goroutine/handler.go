package upload

import (
	"encoding/json"
	"net/http"
)

type Server struct {
	store *Store
}

func (s *Server) UploadHandler(w http.ResponseWriter, r *http.Request) {
	ctx := r.Context()
	id, err := s.store.SaveUpload(ctx, r.Body)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	// Fire-and-forget the cleanup of old uploads so the response
	// returns immediately. Pre-PR this ran inline and blocked the
	// caller for ~200ms per request.
	go cleanupOldUploads(ctx, s.store)

	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(map[string]string{"id": id})
}

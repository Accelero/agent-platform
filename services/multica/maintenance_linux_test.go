package platformauth

import (
	"context"
	"os"
	"path/filepath"
	"syscall"
	"testing"
	"time"
	"github.com/multica-ai/multica/server/internal/service"
)

func TestAgentClaimFencePausesEveryPublicClaimWithoutDatabase(t *testing.T) {
	path := filepath.Join(t.TempDir(), "maintenance")
	t.Setenv("MULTICA_MAINTENANCE_FILE", path)
	if err := os.WriteFile(path, []byte("paused"), 0600); err != nil { t.Fatal(err) }
	s := &service.TaskService{}
	if result, err := s.ClaimTasksForRuntimes(context.Background(), nil, 1); err != nil || result != nil {
		t.Fatal("batch claim was not paused", err)
	}
	// A held shared fence blocks the exclusive publisher until the claim exits.
	release, paused, err := service.AgentClaimFence(context.Background())
	if err != nil || !paused { t.Fatal("maintenance not observed", err) }
	f, err := os.OpenFile(path+".lock", os.O_RDWR, 0600)
	if err != nil { t.Fatal(err) }
	defer f.Close()
	if syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB) == nil {
		t.Fatal("publisher passed an in-flight claim")
	}
	release()
	if err := syscall.Flock(int(f.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil { t.Fatal(err) }
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	if _, _, err := service.AgentClaimFence(ctx); err == nil { t.Fatal("claim ignored its deadline") }
	syscall.Flock(int(f.Fd()), syscall.LOCK_UN)
}

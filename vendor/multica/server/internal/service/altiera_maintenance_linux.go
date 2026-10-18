package service

import (
	"context"
	"errors"
	"fmt"
	"os"
	"syscall"
	"time"
)

// Hold the shared fence through the complete claim transaction. The backup
// process takes it exclusively when publishing maintenance, so it cannot miss
// an in-flight claim or interrupt a running task to obtain its checkpoint.
func AgentClaimFence(ctx context.Context) (func(), bool, error) {
	path := os.Getenv("MULTICA_MAINTENANCE_FILE")
	if path == "" {
		return func() {}, false, nil
	}
	file, err := os.OpenFile(path+".lock", os.O_CREATE|os.O_RDWR, 0600)
	if err != nil {
		return nil, false, fmt.Errorf("maintenance fence unavailable")
	}
	for {
		err = syscall.Flock(int(file.Fd()), syscall.LOCK_SH|syscall.LOCK_NB)
		if err == nil {
			break
		}
		if !errors.Is(err, syscall.EWOULDBLOCK) {
			file.Close()
			return nil, false, fmt.Errorf("maintenance fence unavailable")
		}
		select {
		case <-ctx.Done():
			file.Close()
			return nil, false, ctx.Err()
		case <-time.After(20 * time.Millisecond):
		}
	}
	release := func() {
		syscall.Flock(int(file.Fd()), syscall.LOCK_UN)
		file.Close()
	}
	_, err = os.Stat(path)
	if err != nil && !os.IsNotExist(err) {
		release()
		return nil, false, fmt.Errorf("maintenance state unavailable")
	}
	return release, err == nil, nil
}

package main

import (
	"context"
	"regexp"
	"testing"
)

func TestNativeCredentialGeneration(t *testing.T) {
	out, err := run(request{Action: "generate"})
	if err != nil {
		t.Fatal("generation failed")
	}
	pattern := regexp.MustCompile(`^mul_[0-9a-f]{40}$`)
	if !pattern.MatchString(out.Daemon) || !pattern.MatchString(out.Profile) || out.Daemon == out.Profile {
		t.Fatal("invalid native credentials")
	}
}
func TestInvalidInputRejectedBeforeDatabaseWrites(t *testing.T) {
	if _, err := ensure(context.Background(), nil, request{}); err == nil {
		t.Fatal("accepted empty input")
	}
}
func TestUUIDRoundTrip(t *testing.T) {
	value := "00000000-0000-0000-0000-000000000001"
	if idString(uuid(value)) != value {
		t.Fatal("identity changed")
	}
}

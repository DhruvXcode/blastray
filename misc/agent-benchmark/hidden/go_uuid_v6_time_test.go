package uuid

import (
	"testing"
	"time"
)

func TestM22HiddenV6PreservesTimestamp(t *testing.T) {
	oldNow, oldLast, oldSeq := timeNow, lasttime, clockSeq
	defer func() { timeNow, lasttime, clockSeq = oldNow, oldLast, oldSeq }()
	timeNow = func() time.Time { return time.Unix(1_712_345_678, 901_234_500) }
	lasttime = 0
	clockSeq = 0x9234

	want := Time(uint64(timeNow().UnixNano()/100) + g1582ns100)
	got, err := NewV6()
	if err != nil {
		t.Fatal(err)
	}
	if got.Time() != want {
		t.Fatalf("v6 Time() = %d, want %d", got.Time(), want)
	}
}

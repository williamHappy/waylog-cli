"""Offline-testable scheduling. No task registration or network operations."""
import datetime as dt
import calendar
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError


class ScheduleError(ValueError):
    pass


class Schedule:
    def __init__(self, spec=None, interval=3600):
        self.spec = dict(spec or {"mode": "interval", "seconds": interval})
        self.mode = self.spec.get("mode")
        if self.mode == "interval":
            if set(self.spec) != {"mode", "seconds"} or type(self.spec["seconds"]) is not int or self.spec["seconds"] < 60:
                raise ScheduleError("interval requires integer seconds >= 60")
            self.seconds = self.spec["seconds"]
            self.zone = None
        elif self.mode == "cron":
            if set(self.spec) != {"mode", "expression", "timezone"}:
                raise ScheduleError("cron requires expression and explicit IANA timezone")
            try:
                self.zone = ZoneInfo(self.spec["timezone"])
            except (ZoneInfoNotFoundError, TypeError) as error:
                raise ScheduleError("unknown timezone; Windows requires an installed tzdata database") from error
            if not isinstance(self.spec["expression"], str) or len(self.spec["expression"].split()) != 5:
                raise ScheduleError("cron requires five numeric fields")
            fields = self.spec["expression"].split()
            self.fields = [self.parse_field(x, lo, hi) for x, lo, hi in zip(fields, (0, 0, 1, 1, 0), (59, 23, 31, 12, 7))]
            self.fields[4] = {0 if x == 7 else x for x in self.fields[4]}
            self.day_wild = fields[2].split('/')[0] == '*'
            self.week_wild = fields[4].split('/')[0] == '*'
            if self.week_wild and not self.day_wild and not any(day <= calendar.monthrange(2024, month)[1] for month in self.fields[3] for day in self.fields[2]):
                raise ScheduleError('cron day/month combination can never occur')
        else:
            raise ScheduleError("mode must be interval or cron")

    @staticmethod
    def parse_field(field, low, high):
        result = set()
        try:
            for item in field.split(','):
                if not item or item.count('/') > 1:
                    raise ValueError()
                base, *step_text = item.split('/')
                step = int(step_text[0]) if step_text else 1
                if step < 1:
                    raise ValueError()
                if base == '*':
                    first, last = low, high
                elif '-' in base:
                    bounds = base.split('-')
                    if len(bounds) != 2 or not all(x.isdecimal() for x in bounds):
                        raise ValueError()
                    first, last = map(int, bounds)
                else:
                    if not base.isdecimal():
                        raise ValueError()
                    first = int(base)
                    last = high if step_text else first
                if not low <= first <= last <= high:
                    raise ValueError()
                result.update(range(first, last + 1, step))
        except ValueError as error:
            raise ScheduleError("invalid numeric cron field: " + field) from error
        return result

    def matches(self, epoch):
        local = dt.datetime.fromtimestamp(epoch, self.zone)
        minute, hour, day, month, weekday = self.fields
        dom = local.day in day
        dow = (local.weekday() + 1) % 7 in weekday
        day_ok = (dom and dow) if self.day_wild or self.week_wild else (dom or dow)
        return local.minute in minute and local.hour in hour and local.month in month and day_ok

    def next_after(self, epoch):
        if self.mode == 'interval':
            return epoch + self.seconds
        # Iterate UTC minutes so DST gaps/folds and offset boundaries are explicit.
        candidate = (int(epoch) // 60 + 1) * 60
        for _ in range(60 * 24 * 366 * 8):
            if self.matches(candidate):
                return float(candidate)
            candidate += 60
        raise ScheduleError("cron has no trigger in eight years")

    def due_slot(self, now, last_slot=None):
        if self.mode == 'interval':
            slot = int(now) // self.seconds * self.seconds
            if last_slot is None or slot > last_slot:
                return float(slot)
            return None
        minute = int(now) // 60 * 60
        # Coalesce delayed execution into one slot. No replay of every missed run.
        floor = int(last_slot) if last_slot is not None else minute - 60
        candidate = minute
        while candidate > floor:
            if self.matches(candidate):
                return float(candidate)
            if minute - candidate > 366 * 24 * 3600:
                return None
            candidate -= 60
        return None

    def native_spec(self, platform):
        if platform not in ('macos', 'windows'):
            raise ScheduleError('unsupported platform')
        return {"platform": platform, "executor": "v2-dispatch", "pollSeconds": 60 if self.mode == 'cron' else self.seconds,
                "schedule": self.spec, "singleInstance": True, "coalesceMissed": True,
                "watchIndependent": True, "registrationPerformed": False}

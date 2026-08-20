#!/usr/bin/perl
use strict;
use warnings;
use IO::Select;
use Time::HiRes qw(time);

my $saved_mode = `stty -g`;
chomp $saved_mode;
system('stty', 'raw', '-echo');
$| = 1;

sub restore_terminal {
    system('stty', $saved_mode) if $saved_mode ne '';
    print "\e[?25h\e[0m\r\n";
    exit 0;
}

$SIG{INT} = \&restore_terminal;
$SIG{TERM} = \&restore_terminal;
$SIG{HUP} = \&restore_terminal;

my @spinner = qw(| / - \\);
my $spin = 0;
my $input = '';
my $next_frame = time();
my $reader = IO::Select->new(\*STDIN);

print "\e[2J\e[Hagent fixture\e[10;1Hagent> \e[?25h";
while (1) {
    my $now = time();
    my $wait = $next_frame > $now ? $next_frame - $now : 0;
    if ($reader->can_read($wait)) {
        my $bytes = '';
        my $read = sysread(STDIN, $bytes, 1024);
        restore_terminal() if !defined($read) || $read == 0;
        for my $char (split //, $bytes) {
            restore_terminal() if ord($char) == 3;
            if ($char eq "\x7f" || $char eq "\x08") {
                chop $input;
            } elsif ($char eq "\r" || $char eq "\n") {
                $input = '';
            } elsif ($char ge ' ' && $char le '~') {
                $input .= $char;
            }
        }
        print "\e[10;1H\e[2Kagent> $input";
    }

    $now = time();
    if ($now >= $next_frame) {
        print "\e7\e[1;1H\e[2Kagent $spinner[$spin++ % @spinner]\e8";
        $next_frame = $now + (1 / 60);
    }
}

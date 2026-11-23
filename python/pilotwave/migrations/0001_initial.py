from django.db import migrations, models


class Migration(migrations.Migration):
    initial = True

    dependencies = []

    operations = [
        migrations.CreateModel(
            name='PilotwaveHeartbeat',
            fields=[
                ('key', models.CharField(max_length=100, primary_key=True, serialize=False)),
                ('last_seen', models.DateTimeField(auto_now=True)),
                ('metadata', models.JSONField(blank=True, default=dict)),
            ],
            options={
                'db_table': 'pilotwave_heartbeat',
                'verbose_name': 'Pilotwave heartbeat',
                'verbose_name_plural': 'Pilotwave heartbeats',
            },
        ),
    ]

// No preset writes these rows, and changing one keeps the preset.
class ModernMenuMotion extends ModernMenuPage;

function CreateMenuContents()
{
	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	AddBox(0, "ModernFootPlacement");
	AddLevelSlider(1, "ModernHitReactions", 0, 10, 5);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Animation"
	BackHelp="Return to the Modern Graphics menu."

	Labels(0)="Foot IK"
	Labels(1)="Hit reactions"

	Helps(0)="Feet rest on stairs and slopes instead of floating or sinking."
	Helps(1)="How strongly people flinch where they are hit."
}
